#![cfg_attr(not(feature = "std"), no_std)]

use codec::Decode;
use fp_evm::{PrecompileOutput, Context, ExitError};
use frame_support::dispatch::{Dispatchable, GetDispatchInfo, PostDispatchInfo};
use pallet_evm::{Precompile, PrecompileSet};
use pallet_evm_precompile_bn128::{Bn128Add, Bn128Mul, Bn128Pairing};
use pallet_evm_precompile_dispatch::Dispatch;
use pallet_evm_precompile_modexp::Modexp;
use pallet_evm_precompile_simple::{ECRecover, Identity, Ripemd160, Sha256};
use sp_core::H160;
use sp_std::fmt::Debug;
use sp_std::marker::PhantomData;

/// We include the nine Istanbul precompiles
/// (https://github.com/ethereum/go-ethereum/blob/3c46f557/core/vm/contracts.go#L69)
/// as well as a special precompile for dispatching Substrate extrinsics
#[derive(Debug, Clone, Copy)]
pub struct MalanPrecompiles<R>(PhantomData<R>);

impl<R> PrecompileSet for MalanPrecompiles<R>
    where
        R: pallet_evm::Config + xpallet_assets::Config,
        R::Call: Dispatchable<PostInfo = PostDispatchInfo> + GetDispatchInfo + Decode,
        <R::Call as Dispatchable>::Origin: From<Option<R::AccountId>>,
{
    fn execute(
        address: H160,
        input: &[u8],
        target_gas: Option<u64>,
        context: &Context,
    ) -> Option<Result<PrecompileOutput, ExitError>> {
        match address {
            // Ethereum precompiles
            a if a == hash(1) => Some(ECRecover::execute(input, target_gas, context)),
            a if a == hash(2) => Some(Sha256::execute(input, target_gas, context)),
            a if a == hash(3) => Some(Ripemd160::execute(input, target_gas, context)),
            a if a == hash(4) => Some(Identity::execute(input, target_gas, context)),
            a if a == hash(5) => Some(Modexp::execute(input, target_gas, context)),
            a if a == hash(6) => Some(Bn128Add::execute(input, target_gas, context)),
            a if a == hash(7) => Some(Bn128Mul::execute(input, target_gas, context)),
            a if a == hash(8) => Some(Bn128Pairing::execute(input, target_gas, context)),
            // Non Ethereum precompiles
            a if a == hash(1024) => Some(Dispatch::<R>::execute(input, target_gas, context)),
            a if a == hash(1025) => Some(sbtc::Sbtc::<R>::execute(input, target_gas, context)),

            _ => None,
        }
    }
}

fn hash(a: u64) -> H160 {
    H160::from_low_u64_be(a)
}


pub mod sbtc {
    use core::marker::PhantomData;
    use codec::{Encode, Decode};
    use sp_core::{H160, U256, hexdisplay::HexDisplay};
    use sp_runtime::{traits::UniqueSaturatedInto, AccountId32};
    use sp_std::vec::Vec;
    use frame_support::log;
    use frame_support::traits::{Currency, ExistenceRequirement};
    use ethabi::Token;
    use fp_evm::{PrecompileOutput, Context, ExitError, ExitSucceed};
    use pallet_evm::{Precompile, AddressMapping};
    use orml_traits::MultiCurrency;
    use crate::{AssetId, PCX, X_BTC};

    pub type BalanceOf<T> =
    <<T as xpallet_assets::Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;

    pub struct Sbtc<T: pallet_evm::Config + xpallet_assets::Config> {
        _marker: PhantomData<T>,
    }

    impl<T: pallet_evm::Config + xpallet_assets::Config> Sbtc<T>
    {
        fn process(
            input: &[u8]
        ) -> Result<Vec<u8>, ExitError>{
            match input.len() {
                // withdraw pcx
                // input = from(evm address, 20 bytes) + to(substrate pubkey, 32 bytes) + value(32 bytes)
                84 => {
                    Self::process_withdraw(input, PCX)
                        .map(Self::encode_success)
                        .map_err(|err| {
                            log::warn!(target: "evm", "withdraw pcx: err = {:?}", err);
                            err
                        })
                },
                // withdraw sbtc
                // input = from(evm address, 20 bytes) + to(substrate pubkey, 32 bytes) + value(32 bytes) + padding(1 byte)
                85 => {
                   Self::process_withdraw(input, X_BTC)
                       .map(Self::encode_success)
                       .map_err(|err| {
                           log::warn!(target: "evm", "withdraw sbtc: err = {:?}", err);
                           err
                       })
                },
                // transfer sbtc
                // input = from(evm address, 20 bytes) + to(evm address, 20 bytes) + value(32 bytes)
                72 => {
                    Self::process_transfer(input)
                        .map(Self::encode_success)
                        .map_err(|err| {
                            log::warn!(target: "evm", "transfer sbtc: err = {:?}", err);
                            err
                        })

                },
                // sbtc free balance
                // input = from(evm address, 20 bytes)
                20 => {
                    let balance = Self::process_free_balance(input)
                        .map_err(|err| {
                            log::warn!(target: "evm", "get free balance: err = {:?}", err);
                            err
                        })?;

                    Ok(Self::encode_u128(balance))
                },
                // sbtc total balance
                // input = from(evm address, 20 bytes) + padding(1 byte)
                21 => {
                    let balance = Self::process_total_balance(input)
                        .map_err(|err| {
                            log::warn!(target: "evm", "get total balance: err = {:?}", err);
                            err
                        })?;

                    Ok(Self::encode_u128(balance))
                },
                _ => {
                    log::warn!(target: "evm", "invalid input: {:?}", input);

                    Err(ExitError::Other("invalid input".into()))
                }
            }
        }

        fn account_from_address(
            address: &[u8]
        ) -> Result<T::AccountId, ExitError> {
            frame_support::ensure!(address.len() == 20, ExitError::Other("invalid address".into()));

            let from = H160::from_slice(&address[0..20]);

            Ok(T::AddressMapping::into_account_id(from))
        }

        fn account_from_pubkey(
            pubkey: &[u8]
        ) -> Result<T::AccountId, ExitError> {
            frame_support::ensure!(pubkey.len() == 32, ExitError::Other("invalid pubkey".into()));

            let mut target = [0u8; 32];
            target[0..32].copy_from_slice(&pubkey[0..32]);

            T::AccountId::decode(&mut &AccountId32::new(target).encode()[..])
                .map_err(|_| ExitError::Other("decode AccountId32 failed".into()))
        }

        fn balance(value: &[u8]) -> Result<u128, ExitError> {
            frame_support::ensure!(value.len() == 32, ExitError::Other("invalid balance".into()));

            Ok(U256::from_big_endian(&value[0..32]).low_u128())
        }

        fn process_withdraw(
            input: &[u8],
            id: AssetId
        ) -> Result<bool, ExitError> {
            let from = Self::account_from_address(&input[0..20])?;
            let to = Self::account_from_pubkey(&input[20..52])?;
            let balance = Self::balance(&input[52..84])?;

            log::debug!(target: "evm", "from(evm): {:?}", H160::from_slice(&input[0..20]));
            log::debug!(target: "evm", "from(sub): {:?}", HexDisplay::from(&from.encode()));
            log::debug!(target: "evm", "to(sub): {:?}", HexDisplay::from(&to.encode()));
            log::debug!(target: "evm", "value(sub): {:?}", balance);



            Self::multi_transfer(id, &from, &to, balance.unique_saturated_into())
        }

        fn process_transfer(
            input: &[u8],
        ) -> Result<bool, ExitError> {
            let from = Self::account_from_address(&input[0..20])?;
            let to = Self::account_from_address(&input[20..40])?;
            let balance = Self::balance(&input[40..72])?;

            log::debug!(target: "evm", "from(evm): {:?}", H160::from_slice(&input[0..20]));
            log::debug!(target: "evm", "from(sub): {:?}", HexDisplay::from(&from.encode()));
            log::debug!(target: "evm", "to(sub): {:?}", HexDisplay::from(&to.encode()));
            log::debug!(target: "evm", "value(sub): {:?}", balance);



            Self::multi_transfer(X_BTC, &from, &to, balance.unique_saturated_into())
        }

        fn process_free_balance(
            input: &[u8],
        ) -> Result<u128, ExitError> {
            let from = Self::account_from_address(&input[0..20])?;

            log::debug!(target: "evm", "from(evm): {:?}", H160::from_slice(&input[0..20]));
            log::debug!(target: "evm", "from(sub): {:?}", HexDisplay::from(&from.encode()));

            let balance: u128 = <xpallet_assets::Pallet<T> as MultiCurrency<T::AccountId>>::free_balance(
                PCX,
                &from,
            ).unique_saturated_into();

            Ok(balance)
        }

        fn process_total_balance(
            input: &[u8],
        ) -> Result<u128, ExitError> {
            let from = Self::account_from_address(&input[0..20])?;

            log::debug!(target: "evm", "from(evm): {:?}", H160::from_slice(&input[0..20]));
            log::debug!(target: "evm", "from(sub): {:?}", HexDisplay::from(&from.encode()));

            let balance: u128 = <xpallet_assets::Pallet<T> as MultiCurrency<T::AccountId>>::total_balance(
                X_BTC,
                &from,
            ).unique_saturated_into();

            Ok(balance)
        }

        fn multi_transfer(
            id: AssetId,
            from: &T::AccountId,
            to: &T::AccountId,
            amount: BalanceOf<T>
        ) -> Result<bool, ExitError> {
            match id {
                PCX => {
                    let free = <T as xpallet_assets::Config>::Currency::free_balance(from);
                    let min = <T as xpallet_assets::Config>::Currency::minimum_balance();

                    if free < amount || amount < min {
                        return Ok(false)
                    }

                    <T as xpallet_assets::Config>::Currency::transfer(
                        from,
                        to,
                        amount,
                        ExistenceRequirement::AllowDeath,
                    )
                        .map(|_| true)
                        .map_err(|err| {
                            ExitError::Other(sp_std::borrow::Cow::Borrowed(err.into()))
                        })
                },
                X_BTC => {
                    let free = <xpallet_assets::Pallet<T> as MultiCurrency<T::AccountId>>::free_balance(X_BTC, &from);
                    let min = <xpallet_assets::Pallet<T> as MultiCurrency<T::AccountId>>::minimum_balance(X_BTC);

                    if free < amount || amount < min {
                        return Ok(false)
                    }

                    <xpallet_assets::Pallet<T> as MultiCurrency<T::AccountId>>::transfer(
                        X_BTC,
                        from,
                        to,
                        amount
                    )
                        .map(|_| true)
                        .map_err(|err| {
                            ExitError::Other(sp_std::borrow::Cow::Borrowed(err.into()))
                        })
                }
                _ => {
                    // do nothing
                    Ok(true)
                }
            }

        }

        fn encode_success(success: bool) -> Vec<u8> {
            let out = Token::Bool(success);
            ethabi::encode(&[out])
        }

        fn encode_u128(balance: u128) -> Vec<u8> {
            let out = Token::Uint(U256::from(balance));
            ethabi::encode(&[out])
        }
    }

    impl<T> Precompile for Sbtc<T>
    where
        T: pallet_evm::Config + xpallet_assets::Config,
        T::AccountId: Decode,
    {
        fn execute(
            input: &[u8],
            _target_gas: Option<u64>,
            context: &Context,
        ) -> Result<PrecompileOutput, ExitError> {

            log::debug!(target: "evm", "caller: {:?}", context.caller);

            const BASE_GAS_COST: u64 = 45_000;

            Ok(PrecompileOutput {
                exit_status: ExitSucceed::Returned,
                cost: BASE_GAS_COST,
                output: Self::process(input)?,
                logs: Default::default(),
            })
        }
    }

}
