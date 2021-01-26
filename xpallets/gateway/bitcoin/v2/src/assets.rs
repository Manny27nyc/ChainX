#![cfg_attr(not(feature = "std"), no_std)]

#[frame_support::pallet]
#[allow(dead_code)]
pub mod pallet {
    use sp_std::{convert::TryInto, marker::PhantomData};

    #[cfg(feature = "std")]
    use frame_support::traits::GenesisBuild;

    use frame_support::{
        dispatch::{DispatchError, DispatchResult},
        storage::types::{StorageValue, ValueQuery},
        traits::{Currency, Hooks, ReservableCurrency},
    };
    use frame_system::pallet_prelude::BlockNumberFor;

    pub type BalanceOf<T> = <<T as xpallet_assets::Config>::Currency as Currency<
        <T as frame_system::Config>::AccountId,
    >>::Balance;

    #[pallet::pallet]
    #[pallet::generate_store(pub(crate) trait Store)]
    pub struct Pallet<T>(PhantomData<T>);

    #[pallet::config]
    pub trait Config: frame_system::Config + xpallet_assets::Config {}

    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {}

    #[pallet::call]
    impl<T: Config> Pallet<T> {}

    #[pallet::error]
    pub enum Error<T> {
        /// Requester doesn't has enough pcx for collateral.
        InsufficientFunds,
        /// Calculation during exchangine was overflow
        ArithmeticOverflow,
        /// Calculation during exchangine was underflow
        ArithmeticUnderflow,
        /// Cannot convert into `BalanceOf<T>`
        TryIntoError,
    }

    /// Total collateral
    #[pallet::storage]
    #[pallet::getter(fn total_collateral)]
    pub(crate) type TotalCollateral<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    /// Exchange rate in integer from btc to pcx
    #[pallet::storage]
    #[pallet::getter(fn exchange_rate)]
    pub(crate) type ExchangeRate<T: Config> = StorageValue<_, u128, ValueQuery>;

    /// Exchange rate's decimal
    #[pallet::storage]
    #[pallet::getter(fn exchange_rate_decimal)]
    pub(crate) type ExchangeRateDecimal<T: Config> = StorageValue<_, u8, ValueQuery>;

    #[pallet::genesis_config]
    #[derive(Default)]
    pub struct GenesisConfig {
        /// Exchange rate from btc to pcx. It means how many pcx tokens could 1 btc excahnge.
        /// For example, suppose 1BTC = 1234.56789123PCX, then
        /// exchange_rate = 123456789123
        /// decimal = 8
        pub exchange_rate: u128,
        /// Exchange rate decimal
        pub exchange_rate_decimal: u8,
    }

    #[pallet::genesis_build]
    impl<T: Config> GenesisBuild<T> for GenesisConfig {
        fn build(&self) {
            <ExchangeRate<T>>::put(self.exchange_rate);
            <ExchangeRateDecimal<T>>::put(self.exchange_rate_decimal);
        }
    }

    impl<T: Config> Pallet<T> {
        fn into_u128<I: TryInto<u128>>(x: I) -> Result<u128, DispatchError> {
            TryInto::<u128>::try_into(x).map_err(|_| Error::<T>::TryIntoError.into())
        }
        pub fn btc_to_pcx(amount: BalanceOf<T>) -> Result<BalanceOf<T>, DispatchError> {
            let raw_amount = Self::into_u128(amount)?;
            let decimal = 10_u128.pow(u32::from(Self::exchange_rate_decimal()));
            let raw_pcx = Self::exchange_rate()
                .checked_mul(raw_amount)
                .ok_or(Error::<T>::ArithmeticOverflow)?
                .checked_div(decimal)
                .ok_or(Error::<T>::ArithmeticUnderflow)?;
            let result = raw_pcx.try_into().map_err(|_| Error::<T>::TryIntoError)?;
            Ok(result)
        }

        pub fn pcx_to_btc(amount: BalanceOf<T>) -> Result<BalanceOf<T>, DispatchError> {
            let raw_amount = Self::into_u128(amount)?;
            let decimal = 10_u128.pow(u32::from(Self::exchange_rate_decimal()));
            let raw_btc = raw_amount
                .checked_mul(decimal)
                .ok_or(Error::<T>::ArithmeticOverflow)?
                .checked_div(Self::exchange_rate())
                .ok_or(Error::<T>::ArithmeticUnderflow)?;
            let result = raw_btc.try_into().map_err(|_| Error::<T>::TryIntoError)?;
            Ok(result)
        }

        /// Lock collateral
        #[inline]
        pub fn lock_collateral(sender: &T::AccountId, amount: BalanceOf<T>) -> DispatchResult {
            <<T as xpallet_assets::Config>::Currency as ReservableCurrency<
                <T as frame_system::Config>::AccountId,
            >>::reserve(sender, amount)
            .map_err(|_| Error::<T>::InsufficientFunds)?;
            Ok(())
        }

        /// increase total collateral
        #[inline]
        pub fn increase_total_collateral(amount: BalanceOf<T>) {
            <TotalCollateral<T>>::mutate(|c| *c += amount);
        }
    }
}
