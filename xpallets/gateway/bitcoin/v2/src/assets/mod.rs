#![cfg_attr(not(feature = "std"), no_std)]

/// Types used by pallet
pub mod types;

/// Manage exchanging between assets
#[frame_support::pallet]
#[allow(dead_code)]
pub mod pallet {
    use sp_arithmetic::traits::SaturatedConversion;
    use sp_std::{default::Default, marker::PhantomData, vec::Vec};

    #[cfg(feature = "std")]
    use frame_support::traits::GenesisBuild;
    use frame_support::{
        dispatch::{DispatchError, DispatchResult, DispatchResultWithPostInfo},
        ensure,
        storage::types::{StorageValue, ValueQuery},
        traits::{Currency, Hooks, IsType, ReservableCurrency},
    };
    use frame_system::{
        ensure_root, ensure_signed,
        pallet_prelude::{BlockNumberFor, OriginFor},
    };

    use super::types::{ErrorCode, Status, TradingPrice};

    pub type BalanceOf<T> = <<T as xpallet_assets::Config>::Currency as Currency<
        <T as frame_system::Config>::AccountId,
    >>::Balance;

    pub type CurrencyOf<T> = <T as xpallet_assets::Config>::Currency;

    #[pallet::pallet]
    #[pallet::generate_store(pub(crate) trait Store)]
    pub struct Pallet<T>(PhantomData<T>);

    #[pallet::config]
    pub trait Config: frame_system::Config + xpallet_assets::Config {
        type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;
    }

    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
        fn on_initialize(n: BlockNumberFor<T>) -> frame_support::weights::Weight {
            let height = Self::exchange_rate_update_time();
            let period = Self::exchange_rate_expired_period();
            if n - height > period {
                <BridgeStatus<T>>::put(Status::Error(ErrorCode::EXCHANGE_RATE_EXPIRED));
            };
            0u64.into()
        }

        fn on_finalize(_: BlockNumberFor<T>) {
            // recover from error if all errors were solved.
            if let Status::Error(ErrorCode::NONE) = Self::bridge_status() {
                <BridgeStatus<T>>::put(Status::Running);
            }
        }
    }

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// Update exchange rate by oracle.
        #[pallet::weight(0)]
        pub(crate) fn update_exchange_rate(
            origin: OriginFor<T>,
            exchange_rate: TradingPrice,
        ) -> DispatchResultWithPostInfo {
            let sender = ensure_signed(origin)?;
            ensure!(Self::is_oracle(&sender), Error::<T>::OperationForbidden);
            Self::_update_exchange_rate(exchange_rate.clone())?;
            Self::deposit_event(Event::<T>::ExchangeRateUpdated(sender, exchange_rate));
            Ok(().into())
        }

        /// Force update the exchange rate.
        #[pallet::weight(0)]
        pub(crate) fn force_update_exchange_rate(
            origin: OriginFor<T>,
            exchange_rate: TradingPrice,
        ) -> DispatchResultWithPostInfo {
            ensure_root(origin)?;
            Self::_update_exchange_rate(exchange_rate.clone())?;
            Self::deposit_event(Event::<T>::ExchangeRateForceUpdated(exchange_rate));
            Ok(().into())
        }

        /// Force update oracles.
        #[pallet::weight(0)]
        pub(crate) fn force_update_oracles(
            origin: OriginFor<T>,
            oracles: Vec<T::AccountId>,
        ) -> DispatchResultWithPostInfo {
            ensure_root(origin)?;
            OracleAccounts::<T>::put(oracles.clone());
            Self::deposit_event(Event::<T>::OracleForceUpdated(oracles));
            Ok(().into())
        }
    }

    /// Errors for assets module
    #[pallet::error]
    pub enum Error<T> {
        /// Permission denied.
        OperationForbidden,
        /// Requester doesn't have enough pcx for collateral.
        InsufficientFunds,
        /// Arithmetic underflow/overflow.
        ArithmeticError,
        /// Account doesn't have enough collateral to be slashed.
        InsufficientCollateral,
        /// Bridge was shutdown or in error.
        BridgeNotRunning,
    }

    /// Events for assets module
    #[pallet::event]
    #[pallet::generate_deposit(pub(crate) fn deposit_event)]
    pub enum Event<T: Config> {
        /// Update exchange rate by oracle
        ExchangeRateUpdated(T::AccountId, TradingPrice),
        /// Update exchange rate by root
        ExchangeRateForceUpdated(TradingPrice),
        /// Update oracles by root
        OracleForceUpdated(Vec<T::AccountId>),
    }

    /// Total collateral
    #[pallet::storage]
    #[pallet::getter(fn total_collateral)]
    pub(crate) type TotalCollateral<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    /// Exchange rate from btc to pcx
    #[pallet::storage]
    #[pallet::getter(fn exchange_rate)]
    pub(crate) type ExchangeRate<T: Config> = StorageValue<_, TradingPrice, ValueQuery>;

    #[pallet::storage]
    #[pallet::getter(fn oracle_accounts)]
    pub(crate) type OracleAccounts<T: Config> = StorageValue<_, Vec<T::AccountId>, ValueQuery>;

    #[pallet::storage]
    #[pallet::getter(fn bridge_status)]
    pub(crate) type BridgeStatus<T: Config> = StorageValue<_, Status, ValueQuery>;

    #[pallet::storage]
    #[pallet::getter(fn exchange_rate_update_time)]
    pub(crate) type ExchangeRateUpdateTime<T: Config> =
        StorageValue<_, BlockNumberFor<T>, ValueQuery>;

    #[pallet::storage]
    #[pallet::getter(fn exchange_rate_expired_period)]
    pub(crate) type ExchangeRateExpiredPeriod<T: Config> =
        StorageValue<_, BlockNumberFor<T>, ValueQuery>;

    #[pallet::genesis_config]
    pub struct GenesisConfig<T: Config> {
        /// pcx/btc trading pair
        pub exchange_rate: TradingPrice,
        /// oracles allowed to update exchange_rate
        pub oracle_accounts: Vec<T::AccountId>,
    }

    #[cfg(feature = "std")]
    impl<T: Config> Default for GenesisConfig<T> {
        fn default() -> Self {
            Self {
                exchange_rate: Default::default(),
                oracle_accounts: Default::default(),
            }
        }
    }

    #[pallet::genesis_build]
    impl<T: Config> GenesisBuild<T> for GenesisConfig<T> {
        fn build(&self) {
            <ExchangeRate<T>>::put(self.exchange_rate.clone());
            <OracleAccounts<T>>::put(self.oracle_accounts.clone());
        }
    }

    impl<T: Config> Pallet<T> {
        pub fn convert_to_pcx(btc_amount: BalanceOf<T>) -> Result<BalanceOf<T>, DispatchError> {
            //TODO(wangyafei): add lower bound?
            let exchange_rate = Self::exchange_rate();
            let result = exchange_rate
                .convert_to_pcx(btc_amount.saturated_into())
                .ok_or(Error::<T>::ArithmeticError)?;
            Ok(result.saturated_into())
        }
        /// Lock collateral
        #[inline]
        pub fn lock_collateral(sender: &T::AccountId, amount: BalanceOf<T>) -> DispatchResult {
            <<T as xpallet_assets::Config>::Currency as ReservableCurrency<
                <T as frame_system::Config>::AccountId,
            >>::reserve(sender, amount)
            .map_err(|_| Error::<T>::InsufficientFunds)?;
            <TotalCollateral<T>>::mutate(|total| *total += amount);
            Ok(())
        }

        /// increase total collateral
        #[inline]
        pub fn increase_total_collateral(amount: BalanceOf<T>) {
            <TotalCollateral<T>>::mutate(|c| *c += amount);
        }

        #[inline]
        pub(crate) fn is_oracle(account: &T::AccountId) -> bool {
            let oracles: Vec<T::AccountId> = Self::oracle_accounts();
            oracles.contains(account)
        }

        pub(crate) fn _update_exchange_rate(exchange_rate: TradingPrice) -> DispatchResult {
            // TODO: sanity check?
            <ExchangeRate<T>>::put(exchange_rate);
            let height = <frame_system::Pallet<T>>::block_number();
            <ExchangeRateUpdateTime<T>>::put(height);
            Self::recover_from_exchange_rate_expired();
            Ok(())
        }

        /// Slash collateral to receiver
        pub fn slash_collateral(
            sender: &T::AccountId,
            receiver: &T::AccountId,
            amount: BalanceOf<T>,
        ) -> DispatchResult {
            let reserved_collateral = <CurrencyOf<T>>::reserved_balance(sender);
            ensure!(
                reserved_collateral >= amount,
                Error::<T>::InsufficientCollateral
            );
            let (slashed, _) = <CurrencyOf<T>>::slash_reserved(sender, amount);

            <CurrencyOf<T>>::resolve_creating(receiver, slashed);
            <CurrencyOf<T>>::reserve(receiver, amount)
                .map_err(|_| Error::<T>::InsufficientFunds)?;
            //TODO(wangyafei): Self::deposit_event(...);
            Ok(().into())
        }

        /// Release collateral
        pub fn release_collateral(account: &T::AccountId, amount: BalanceOf<T>) -> DispatchResult {
            let reserved_collateral = <CurrencyOf<T>>::reserved_balance(account);
            ensure!(
                reserved_collateral >= amount,
                Error::<T>::InsufficientCollateral
            );
            <CurrencyOf<T>>::unreserve(account, amount);
            <TotalCollateral<T>>::mutate(|total| *total -= amount);
            //TODO(wangyafei): Self::deposit_event(...);
            Ok(())
        }

        /// Get if the bridge running
        pub fn is_bridge_running() -> bool {
            Self::bridge_status() == Status::Running
        }

        pub fn calculate_collateral_ratio(
            issued_tokens: BalanceOf<T>,
            collateral: BalanceOf<T>,
        ) -> Result<u16, DispatchError> {
            let issued_tokens = issued_tokens.saturated_into::<u128>();
            let collateral = collateral.saturated_into::<u128>();
            ensure!(issued_tokens != 0, Error::<T>::ArithmeticError);

            let exchange_rate: TradingPrice = Self::exchange_rate();
            let equivalence_collateral = exchange_rate
                .convert_to_pcx(issued_tokens)
                .ok_or(Error::<T>::ArithmeticError)?;
            let raw_collateral: u128 = collateral.saturated_into();
            let collateral_ratio = raw_collateral
                .checked_mul(100)
                .ok_or(Error::<T>::ArithmeticError)?
                .checked_div(equivalence_collateral)
                .ok_or(Error::<T>::ArithmeticError)?;
            //FIXME(wangyafei): should use try_into?
            Ok(raw_collateral as u16)
        }

        #[inline]
        pub(crate) fn ensure_bridge_running() -> DispatchResult {
            ensure!(
                Self::bridge_status() == Status::Running,
                Error::<T>::BridgeNotRunning
            );
            Ok(())
        }

        /// Clarify `ExchangeRateExpired` is solved and recover from this error.
        ///
        /// Dangerous! Ensure this error truly solved is caller's responsibility.
        pub(crate) fn recover_from_exchange_rate_expired() {
            if let Status::Error(mut error_codes) = Self::bridge_status() {
                if error_codes.contains(ErrorCode::EXCHANGE_RATE_EXPIRED) {
                    error_codes.remove(ErrorCode::EXCHANGE_RATE_EXPIRED);
                    <BridgeStatus<T>>::put(Status::Error(error_codes))
                }
            }
        }

        /// Clarify `Liquidating` is solved and recover from this error.
        ///
        /// Dangerous! Ensure this error truly solved is caller's responsibility.
        pub(crate) fn recover_from_liquidating() {
            if let Status::Error(mut error_codes) = Self::bridge_status() {
                if error_codes.contains(ErrorCode::LIQUIDATING) {
                    error_codes.remove(ErrorCode::LIQUIDATING);
                    <BridgeStatus<T>>::put(Status::Error(error_codes))
                }
            }
        }
    }
}
