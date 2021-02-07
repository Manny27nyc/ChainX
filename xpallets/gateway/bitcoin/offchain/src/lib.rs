// Copyright 2019-2020 ChainX Project Authors. Licensed under GPL-3.0.

//! This module is to support cross-chain bitcoin transactions.
//!
//! ## Overview
//! The main things in this module:
//! - Submit header
//! - Submit transaction
//! - Broadcast raw transaction
//!
//! ### Submit header
//! Fetch block headers from btc network and submit them to ChainX.
//!
//! ### Submit transaction
//! Fetch transactions from btc network and submit filtered deposit/withdrawal transactions of X-BTC.
//!
//! ### Broadcast raw transaction
//! Get withdrawal transactions from ChainX and broadcast raw transaction to btc network.
//!

// Ensure we're `no_std` when compiling for Wasm.
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{format, string::String};

mod request;

#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

use frame_support::{
    debug, decl_error, decl_event, decl_module, decl_storage,
    dispatch::{DispatchResultWithPostInfo, Parameter},
    traits::Get,
    weights::Pays,
    StorageValue,
};

use frame_system::{
    ensure_signed,
    offchain::{
        AppCrypto, CreateSignedTransaction, SendSignedTransaction, SendTransactionTypes, Signer,
    },
};
use sp_application_crypto::RuntimeAppPublic;
use sp_core::{crypto::KeyTypeId, offchain::Duration};
use sp_io::offchain;
use sp_runtime::{
    offchain::{
        http::PendingRequest,
        storage::StorageValueRef,
        storage_lock::{StorageLock, Time},
    },
    transaction_validity::{
        InvalidTransaction, TransactionPriority, TransactionSource, TransactionValidity,
        ValidTransaction,
    },
};

use sp_std::{
    collections::btree_set::BTreeSet, convert::TryFrom, marker::PhantomData, str, vec::Vec,
};

use light_bitcoin::{
    chain::{Block as BtcBlock, BlockHeader as BtcHeader, Transaction as BtcTransaction},
    keys::{Address as BtcAddress, Network as BtcNetwork},
    merkle::PartialMerkleTree,
    primitives::{hash_rev, H256 as BtcHash},
    serialization::serialize,
};
use request::{MAX_RETRY_NUM, RETRY_NUM};
use xp_gateway_bitcoin::{BtcTxMetaType, BtcTxTypeDetector, OpReturnExtractor};
use xp_gateway_common::AccountExtractor;
use xpallet_assets::Chain;
use xpallet_gateway_bitcoin::{
    types::{BtcRelayedTxInfo, BtcTxResult, VoteResult},
    Module as XGatewayBitcoin, WeightInfo,
};
use xpallet_gateway_common::{trustees::bitcoin::BtcTrusteeAddrInfo, Module as XGatewayCommon};

// Max worker nums
const MAX_WORKER_NUM: usize = 1;
const DEFAULT_WORKER_NUM: usize = 1;
// Default delay
const DEFAULT_DELAY: Duration = Duration::from_millis(6_000);

/// Defines application identifier for crypto keys of this module.
///
/// Every module that deals with signatures needs to declare its unique identifier for
/// its crypto keys.
/// When offchain worker is signing transactions it's going to request keys of type
/// `KeyTypeId` from the keystore and use the ones it finds to sign the transaction.
/// The keys can be inserted manually via RPC (see `author_insertKey`).
pub const BTC_RELAY: KeyTypeId = KeyTypeId(*b"btcr");

/// Based on the above `KeyTypeId` we need to generate a pallet-specific crypto type wrappers.
/// We can use from supported crypto kinds (`sr25519`, `ed25519` and `ecdsa`) and augment
/// the types with this pallet-specific identifier.
pub mod app {
    pub use super::BTC_RELAY;
    use crate::AuthorityId;
    use sp_core::sr25519::Signature as Sr25519Signature;
    use sp_runtime::app_crypto::{app_crypto, sr25519};
    use sp_runtime::{traits::Verify, MultiSignature, MultiSigner};

    app_crypto!(sr25519, BTC_RELAY);

    impl frame_system::offchain::AppCrypto<MultiSigner, MultiSignature> for AuthorityId {
        type RuntimeAppPublic = Public;
        type GenericPublic = sp_core::sr25519::Public;
        type GenericSignature = sp_core::sr25519::Signature;
    }

    impl frame_system::offchain::AppCrypto<<Sr25519Signature as Verify>::Signer, Sr25519Signature>
        for AuthorityId
    {
        type RuntimeAppPublic = Public;
        type GenericPublic = sp_core::sr25519::Public;
        type GenericSignature = sp_core::sr25519::Signature;
    }
}

sp_application_crypto::with_pair! {
    /// An bitcoin offchain keypair using sr25519 as its crypto.
    pub type AuthorityPair = app::Pair;
}

/// An bitcoin offchain identifier using sr25519 as its crypto.
pub type AuthorityId = app::Public;

/// An bitcoin offchain signature using sr25519 as its crypto.
pub type AuthoritySignature = app::Signature;

/// This pallet's configuration trait
pub trait Trait:
    SendTransactionTypes<Call<Self>>
    + CreateSignedTransaction<Call<Self>>
    + xpallet_gateway_bitcoin::Trait
    + xpallet_gateway_common::Trait
{
    /// The overarching event type.
    type Event: From<Event<Self>> + Into<<Self as frame_system::Trait>::Event>;
    /// The overarching dispatch call type.
    type Call: From<Call<Self>>;
    /// A configuration for base priority of unsigned transactions.
    type UnsignedPriority: Get<TransactionPriority>;
    /// The identifier type for an offchain worker.
    type AuthorityId: Parameter
        + Default
        + RuntimeAppPublic
        + Ord
        + AppCrypto<Self::Public, Self::Signature>;
    /// Weight information for extrinsics in this pallet.
    type WeightInfo: WeightInfo;
}

decl_event!(
    /// Events generated by the module.
    pub enum Event<T>
    where
        <T as frame_system::Trait>::AccountId
    {
        /// A Bitcoin block generated. [btc_block_height, btc_block_hash]
        NewBtcBlock(u32, BtcHash),
        /// A Bitcoin transaction. [btc_tx_hash]
        NewBtcTransaction(BtcHash),
        _PhantomData(PhantomData::<AccountId>),
    }
);

decl_error! {
    /// Error for the the module
    pub enum Error for Module<T: Trait> {
        /// Offchain HTTP I/O error.
        HttpIoError,
        /// Offchain HTTP deadline reached.
        HttpDeadlineReached,
        /// Offchain HTTP unknown error.
        HttpUnknown,
        /// Offchain HTTP body is not UTF-8.
        HttpBodyNotUTF8,
        /// Bitcoin serialization/deserialization error.
        BtcSserializationError,
        /// Btc send raw transaction rpc error.
        BtcSendRawTxError,
    }
}

impl<T: Trait> From<sp_core::offchain::HttpError> for Error<T> {
    fn from(err: sp_core::offchain::HttpError) -> Self {
        match err {
            sp_core::offchain::HttpError::DeadlineReached => Error::HttpDeadlineReached,
            sp_core::offchain::HttpError::IoError => Error::HttpIoError,
            sp_core::offchain::HttpError::Invalid => Error::HttpUnknown,
        }
    }
}

impl<T: Trait> From<sp_runtime::offchain::http::Error> for Error<T> {
    fn from(err: sp_runtime::offchain::http::Error) -> Self {
        match err {
            sp_runtime::offchain::http::Error::DeadlineReached => Error::HttpDeadlineReached,
            sp_runtime::offchain::http::Error::IoError => Error::HttpIoError,
            sp_runtime::offchain::http::Error::Unknown => Error::HttpUnknown,
        }
    }
}

decl_storage! {
    trait Store for Module<T: Trait> as XGatewayBitcoinOffchain {
        Keys get(fn keys): Vec<T::AuthorityId>;
    }
    add_extra_genesis {
        config(keys): Vec<T::AuthorityId>;
        build(|config| Module::<T>::initialize_keys(&config.keys))
    }
}

decl_module! {
    /// A public part of the pallet.
    pub struct Module<T: Trait> for enum Call where origin: T::Origin {
        fn deposit_event() = default;
        /// Each time the block is imported, a worker thread will be started and run this function.
        fn offchain_worker(block_number: T::BlockNumber) {
            // Worker thread lock
            let mut worker_lock = StorageLock::<'_, Time>::new(b"ocw::worker::lock");
            // Worker thread num
            let worker_num = StorageValueRef::persistent(b"ocw::worker::num");
            // Control worker nums
            {
                let _guard = worker_lock.lock();
                if let Some(Some(num)) = worker_num.get::<u8>(){
                    if num >= MAX_WORKER_NUM as u8 {
                            return;
                    } else {
                            worker_num.set(&(num + 1));
                    }
                } else {
                    worker_num.set(&(DEFAULT_WORKER_NUM as u8));
                }
            }

            // Mainnet or Testnet
            let network = XGatewayBitcoin::<T>::network_id();

            debug::info!("[OCW] Worker[{:?}] Start To Working...", block_number);
            // First, filter transactions from confirmed block and push withdrawal/deposit transactions to chain.
            if Self::get_transactions_and_push(network) {
                // Second, get new block from btc network and push block header to chain
                Self::get_new_header_and_push(network);
                // Delay 6s to prevent submission of the same transaction
                let wait_time = offchain::timestamp().add(DEFAULT_DELAY);
                offchain::sleep_until(wait_time);
                // Finally, get withdrawal proposal from chain and broadcast to btc network.
                match Self::broadcast_withdrawal_proposal(network) {
                    Ok(Some(hash)) => {
                        debug::info!("[OCW|broadcast_withdrawal_proposal] Succeed! Transaction Hash: {:?}", hash);
                    }
                    Ok(None) => {
                        debug::info!("[OCW|broadcast_withdrawal_proposal] No Withdrawal Proposal");
                    }
                    _ => {
                        debug::warn!("[OCW|broadcast_withdrawal_proposal] Failed! Maybe the transaction has been broadcast.");
                    }
                }
            }
            debug::info!("[OCW] Worker[{:?}] Exit.", block_number);
            // Control worker nums
            let _guard = worker_lock.lock();
            if let Some(Some(num)) = worker_num.get::<u8>(){
                worker_num.set(&(num - 1));
            }
        }

        #[weight = <T as Trait>::WeightInfo::push_header()]
        fn push_header(origin, height: u32, header: BtcHeader) -> DispatchResultWithPostInfo {
            let worker = ensure_signed(origin)?;
            debug::info!("[OCW] Worker:{:?} Push Header: {:?}, #Height{}", worker, header, height);
            XGatewayBitcoin::<T>::apply_push_header(header)?;

            Ok(Pays::No.into())
        }

        #[weight = <T as Trait>::WeightInfo::push_transaction()]
        fn push_transaction(origin, tx: BtcTransaction, relayed_info: BtcRelayedTxInfo, prev_tx: Option<BtcTransaction>)  -> DispatchResultWithPostInfo {
            let worker = ensure_signed(origin)?;
            debug::info!("[OCW] Worker:{:?} Push Transaction: {:?}", worker, tx.hash());
            let relay_tx = relayed_info.into_relayed_tx(tx);
            XGatewayBitcoin::<T>::apply_push_transaction(relay_tx, prev_tx)?;

            Ok(Pays::No.into())
        }
    }
}

impl<T: Trait> Module<T> {
    fn initialize_keys(keys: &[T::AuthorityId]) {
        if !keys.is_empty() {
            assert!(Keys::<T>::get().is_empty(), "Keys are already initialized!");
            Keys::<T>::put(keys);
        }
    }
}

/// Most of the functions are moved outside of the `decl_module!` macro.
///
/// This greatly helps with error messages, as the ones inside the macro
/// can sometimes be hard to debug.
impl<T: Trait> Module<T> {
    /// Get withdrawal proposal from chain and broadcast raw transaction
    fn broadcast_withdrawal_proposal(network: BtcNetwork) -> Result<Option<String>, Error<T>> {
        if let Some(withdrawal_proposal) = XGatewayBitcoin::<T>::withdrawal_proposal() {
            if withdrawal_proposal.sig_state == VoteResult::Finish {
                let tx = serialize(&withdrawal_proposal.tx).take();
                let hex_tx = hex::encode(&tx);
                debug::info!("[OCW|send_raw_transaction] Btc Tx Hex: {}", hex_tx);
                match Self::send_raw_transaction(hex_tx, network) {
                    Ok(hash) => {
                        debug::info!(
                            "[OCW|broadcast_withdrawal_proposal] Transaction Hash: {:?}",
                            hash
                        );
                        return Ok(Some(hash));
                    }
                    Err(err) => {
                        debug::warn!("[OCW|broadcast_withdrawal_proposal] Error {:?}", err);
                    }
                }
            }
        }
        Ok(None)
    }

    /// Get new header from btc network and push header to chain
    fn get_new_header_and_push(network: BtcNetwork) {
        let best_index = XGatewayBitcoin::<T>::best_index().height;
        let mut next_height = best_index + 1;
        // Prevent unstable network connections
        for _ in 0..=MAX_RETRY_NUM {
            let btc_block_hash = match Self::fetch_block_hash(next_height, network) {
                Ok(Some(hash)) => {
                    debug::info!("[OCW] ₿ Block #{} hash: {}", next_height, hash);
                    hash
                }
                Ok(None) => {
                    debug::info!("[OCW] ₿ Block #{} has not been generated yet", next_height);
                    // Sleep 5 minutes when there is not new block
                    let sleep_until = offchain::timestamp().add(Duration::from_millis(300_000));
                    offchain::sleep_until(sleep_until);
                    return;
                }
                Err(err) => {
                    debug::warn!("[OCW] ₿ {:?}", err);
                    continue;
                }
            };

            let btc_block = match Self::fetch_block(&btc_block_hash[..], network) {
                Ok(block) => {
                    debug::info!("[OCW] ₿ Block {}", hash_rev(block.hash()));
                    block
                }
                Err(err) => {
                    debug::warn!("[OCW] ₿ {:?}", err);
                    continue;
                }
            };

            let btc_header = btc_block.header;
            // Determine whether the block header already exists
            if let Some(header) = XGatewayBitcoin::<T>::headers(btc_header.hash()) {
                debug::info!("[OCW] Header #{} {:?} Exist.", next_height, header);
                break;
            }
            // Determine whether it is a branch block
            if XGatewayBitcoin::<T>::block_hash_for(best_index)
                .contains(&btc_header.previous_header_hash)
            {
                // Submit a signed transaction
                let signer = Signer::<T, T::AuthorityId>::any_account();
                let result = signer.send_signed_transaction(|_acct| {
                    Call::push_header(next_height, btc_block.header)
                });
                if let Some((_acct, res)) = result {
                    if res.is_err() {
                        debug::warn!(
                            "[OCW|push_header] Failed to submit signed transaction for pushing header: {:?}",
                            res
                        );
                    } else {
                        debug::info!(
                            "[OCW|push_header] ₿ Submitting signed transaction for pushing header: #{}",
                            next_height
                        );
                    }
                }
            } else {
                debug::info!("[OCW|push_header] There is a fork block.");
                next_height -= 1;
                continue;
            }
            break;
        }
    }

    /// Get transactions in confirmed block and push withdrawal/deposit transactions to chain
    fn get_transactions_and_push(network: BtcNetwork) -> bool {
        if let Some(confirmed_index) = XGatewayBitcoin::<T>::confirmed_index() {
            let confirm_height = confirmed_index.height;
            // Get confirmed height from local storage
            let confirmed_info = StorageValueRef::persistent(b"ocw::confirmed");
            // Get confirmed lock
            let mut confirmed_lock = StorageLock::<'_, Time>::new(b"ocw::confirmed::lock");
            // Prevent repeated filtering of transactions
            {
                let _guard = confirmed_lock.lock();
                if let Some(Some(confirmed)) = confirmed_info.get::<u32>() {
                    if confirmed == confirm_height {
                        return true;
                    }
                }
            }

            // Prevent unstable network connections
            for num in 0..=RETRY_NUM {
                if num == MAX_RETRY_NUM {
                    return false;
                }
                let confirm_hash = match Self::fetch_block_hash(confirm_height, network) {
                    Ok(Some(hash)) => {
                        debug::info!("[OCW] ₿ Confirmed Block #{} hash: {}", confirm_height, hash);
                        hash
                    }
                    Ok(None) => {
                        debug::warn!("[OCW] ₿ Confirmed Block #{} Failed", confirm_height);
                        continue;
                    }
                    Err(err) => {
                        debug::warn!("[OCW] ₿ Confirmed {:?}", err);
                        continue;
                    }
                };

                let btc_confirmed_block = match Self::fetch_block(&confirm_hash[..], network) {
                    Ok(block) => {
                        debug::info!("[OCW] ₿ Confirmed Block {}", hash_rev(block.hash()));
                        block
                    }
                    Err(err) => {
                        debug::warn!("[OCW] ₿ Confirmed {:?}", err);
                        continue;
                    }
                };

                if let Ok(yes) = Self::push_xbtc_transaction(&btc_confirmed_block, network) {
                    // Set confirmed height to local storage
                    if yes {
                        confirmed_info.set(&confirm_height);
                        return true;
                    }
                }
                confirmed_info.set(&(confirm_height - 1));
            }
        }
        true
    }

    /// Filter x-btc deposit/withdraw transactions and push to the chain
    fn push_xbtc_transaction(
        confirmed_block: &BtcBlock,
        network: BtcNetwork,
    ) -> Result<bool, Error<T>> {
        // Submitted transaction
        let mut needed = Vec::new();
        // To construct partial merkle tree
        let mut tx_hashes = Vec::with_capacity(confirmed_block.transactions.len());
        let mut tx_matches = Vec::with_capacity(confirmed_block.transactions.len());
        // All requests
        let mut pending_requests = Vec::<PendingRequest>::new();

        // Construct tx detector
        let current_trustee_pair = match Self::get_current_trustee_pair() {
            Ok(Some((hot, cold))) => (hot, cold),
            _ => {
                debug::warn!("[OCW] Can't get current trustee pair!");
                return Ok(false);
            }
        };
        let btc_min_deposit = XGatewayBitcoin::<T>::btc_min_deposit();
        let btc_tx_detector =
            BtcTxTypeDetector::new(network, btc_min_deposit, current_trustee_pair, None);

        // Save all requests
        for tx in confirmed_block.transactions.iter() {
            // Prepare for constructing partial merkle tree
            tx_hashes.push(tx.hash());
            // Skip coinbase (the first transaction of block)
            if tx.is_coinbase() {
                tx_matches.push(false);
                continue;
            }
            let outpoint = tx.inputs[0].previous_output;
            let prev_tx_hash = hex::encode(hash_rev(outpoint.txid));
            let pending = Self::get_transactions_pending(&prev_tx_hash[..], network)?;
            pending_requests.push(pending);
            tx_matches.push(false);
        }

        // Filter transaction type (only deposit and withdrawal)
        let transactions = Self::get_all_transactions(pending_requests)?;
        for (i, prev_tx) in transactions.iter().enumerate() {
            // Skip coinbase
            let tx = &confirmed_block.transactions[i + 1];
            // Detect transaction type
            match btc_tx_detector.detect_transaction_type(
                tx,
                Some(prev_tx),
                OpReturnExtractor::extract_account,
            ) {
                BtcTxMetaType::Withdrawal | BtcTxMetaType::Deposit(..) => {
                    tx_matches[i + 1] = true;
                    needed.push((tx, Some(prev_tx.clone())));
                }
                BtcTxMetaType::HotAndCold
                | BtcTxMetaType::TrusteeTransition
                | BtcTxMetaType::Irrelevance => {}
            }
        }

        // Push x-btc withdraw/deposit transactions if they exist
        if !needed.is_empty() {
            // Construct partial merkle tree
            let merkle_proof = PartialMerkleTree::from_txids(&tx_hashes, &tx_matches);
            // Push xbtc relay (withdraw/deposit) transaction
            let signer = Signer::<T, T::AuthorityId>::any_account();
            for (tx, prev_tx) in needed {
                // Check if the transaction has been completed
                if let Some(state) = XGatewayBitcoin::<T>::tx_state(&tx.hash()) {
                    if state.result == BtcTxResult::Success {
                        continue;
                    }
                }
                let relayed_info = BtcRelayedTxInfo {
                    block_hash: confirmed_block.hash(),
                    merkle_proof: merkle_proof.clone(),
                };
                // Submit a signed transaction
                let result = signer.send_signed_transaction(|_acct| {
                    Call::push_transaction(tx.clone(), relayed_info.clone(), prev_tx.clone())
                });
                if let Some((_acct, res)) = result {
                    if res.is_err() {
                        debug::warn!("[OCW|push_transaction] Failed to submit signed transaction for pushing transaction: {:?}",res);
                    } else {
                        debug::info!(
                            "[OCW|push_transaction] Submitting signed transaction for pushing transaction: #{:?}",
                            tx.hash()
                        );
                    }
                }
            }
        } else {
            debug::info!(
                "[OCW|push_x-btc_transaction] No X-BTC Deposit/Withdraw Transactions in th Confirmed Block {:?}",
                hash_rev(confirmed_block.hash())
            );
        }
        Ok(true)
    }

    /// Get current trustee pair (hot addr and cold addr)
    fn get_current_trustee_pair() -> Result<Option<(BtcAddress, BtcAddress)>, Error<T>> {
        let trustee_session_info_len =
            XGatewayCommon::<T>::trustee_session_info_len(Chain::Bitcoin);
        let current_trustee_session_number = trustee_session_info_len
            .checked_sub(1)
            .unwrap_or(u32::max_value());
        if let Some(trustee_session_info) = XGatewayCommon::<T>::trustee_session_info_of(
            Chain::Bitcoin,
            current_trustee_session_number,
        ) {
            let hot_addr =
                Self::extract_trustee_address(trustee_session_info.0.hot_address).unwrap();
            let cold_addr =
                Self::extract_trustee_address(trustee_session_info.0.cold_address).unwrap();
            Ok(Some((hot_addr, cold_addr)))
        } else {
            Ok(None)
        }
    }

    /// Extract trustee address
    fn extract_trustee_address(address: Vec<u8>) -> Result<BtcAddress, Error<T>> {
        let address = BtcTrusteeAddrInfo::try_from(address).unwrap();
        let address = String::from_utf8(address.addr)
            .unwrap()
            .parse::<BtcAddress>()
            .unwrap();
        Ok(address)
    }
}

impl<T: Trait> sp_runtime::BoundToRuntimeAppPublic for Module<T> {
    type Public = T::AuthorityId;
}

impl<T: Trait> pallet_session::OneSessionHandler<T::AccountId> for Module<T> {
    type Key = T::AuthorityId;

    fn on_genesis_session<'a, I: 'a>(validators: I)
    where
        I: Iterator<Item = (&'a T::AccountId, Self::Key)>,
    {
        let keys = validators.map(|x| x.1).collect::<Vec<_>>();
        Self::initialize_keys(&keys);
    }

    fn on_new_session<'a, I: 'a>(changed: bool, validators: I, queued_validators: I)
    where
        I: Iterator<Item = (&'a T::AccountId, Self::Key)>,
    {
        if changed {
            let keys = validators
                .chain(queued_validators)
                .map(|x| x.1)
                .collect::<BTreeSet<_>>();
            Keys::<T>::put(keys.into_iter().collect::<Vec<_>>());
        }
    }

    fn on_disabled(_validator_index: usize) {}
}

impl<T: Trait> frame_support::unsigned::ValidateUnsigned for Module<T> {
    type Call = Call<T>;

    fn validate_unsigned(_source: TransactionSource, call: &Self::Call) -> TransactionValidity {
        match call {
            Call::push_header(_height, _header) => {
                ValidTransaction::with_tag_prefix("XGatewayBitcoinOffchain")
                .priority(T::UnsignedPriority::get())
                .and_provides("push_header") // TODO: a tag is required, otherwise the transactions will not be pruned.
                // .and_provides((current_session, authority_id)) provide a tag?
                .longevity(1u64) // FIXME a proper longevity
                .propagate(true)
                .build()
            }
            Call::push_transaction(_tx, _relayed_info, _prev_tx) => {
                ValidTransaction::with_tag_prefix("XGatewayBitcoinOffchain")
                .priority(T::UnsignedPriority::get())
                .and_provides("push_transaction") // TODO: a tag is required, otherwise the transactions will not be pruned.
                // .and_provides((current_session, authority_id)) provide a tag?
                .longevity(1u64) // FIXME a proper longevity
                .propagate(true)
                .build()
            }
            _ => InvalidTransaction::Call.into(),
        }
    }
}
