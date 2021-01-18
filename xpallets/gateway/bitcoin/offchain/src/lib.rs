// Copyright 2019-2020 ChainX Project Authors. Licensed under GPL-3.0.

//!

// Ensure we're `no_std` when compiling for Wasm.
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

#[cfg(not(feature = "std"))]
use alloc::{format, string::String};

use frame_support::{
    debug, decl_error, decl_event, decl_module, decl_storage, dispatch::Parameter, traits::Get,
    StorageValue,
};
// use frame_system::{
//     ensure_none,
//     offchain::{CreateSignedTransaction, SendTransactionTypes, SubmitTransaction, Signer, SigningTypes},
//     RawOrigin,
// };
use frame_system::{
    self as system,
    ensure_signed,ensure_none,
    offchain::{
        SendSignedTransaction, SendUnsignedTransaction, SignedPayload, Signer, SigningTypes,
        SubmitTransaction, CreateSignedTransaction, SendTransactionTypes, AppCrypto
    },
    RawOrigin,
};
use codec::{Decode, Encode};
use sp_application_crypto::RuntimeAppPublic;
use sp_core::crypto::KeyTypeId;
use sp_runtime::{
    RuntimeDebug,
    offchain::{http, Duration},
    transaction_validity::{
        InvalidTransaction, TransactionPriority, TransactionSource, TransactionValidity,
        ValidTransaction,
    },
    AccountId32,
};
use sp_std::{collections::btree_set::BTreeSet, marker::PhantomData, str, vec, vec::Vec};

use light_bitcoin::{
    chain::{Block as BtcBlock, Transaction as BtcTransaction},
    keys::Network as BtcNetwork,
    primitives::{hash_rev, H256 as BtcHash},
    serialization::{deserialize, serialize, Reader},
};

use light_bitcoin::merkle::PartialMerkleTree;
use xp_gateway_bitcoin::{
    AccountExtractor, BtcDepositInfo, BtcTxMetaType, BtcTxTypeDetector, OpReturnExtractor,
};
use xpallet_gateway_bitcoin::{
    trustee,
    types::{BtcDepositCache, BtcRelayedTxInfo, VoteResult},
    Module as XGatewayBitcoin,
};
use frame_system::RawOrigin::Signed;

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
mod app {
    pub use super::BTC_RELAY;
    use sp_runtime::app_crypto::{app_crypto, sr25519};
    app_crypto!(sr25519, BTC_RELAY);
}

// pub mod crypto {
// 	use crate::BTC_RELAY;
// 	use sp_core::sr25519::Signature as Sr25519Signature;
// 	use sp_runtime::app_crypto::{app_crypto, sr25519};
// 	use sp_runtime::{
// 		traits::Verify,
// 		MultiSignature, MultiSigner,
// 	};
//
// 	app_crypto!(sr25519, BTC_RELAY);
//
// 	pub struct TestAuthId;
// 	// implemented for ocw-runtime
// 	impl frame_system::offchain::AppCrypto<MultiSigner, MultiSignature> for TestAuthId {
// 		type RuntimeAppPublic = Public;
// 		type GenericSignature = sp_core::sr25519::Signature;
// 		type GenericPublic = sp_core::sr25519::Public;
// 	}
//
// 	// implemented for mock runtime in test
// 	impl frame_system::offchain::AppCrypto<<Sr25519Signature as Verify>::Signer, Sr25519Signature>
// 		for TestAuthId
// 	{
// 		type RuntimeAppPublic = Public;
// 		type GenericSignature = sp_core::sr25519::Signature;
// 		type GenericPublic = sp_core::sr25519::Public;
// 	}
// }

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
{  /// The overarching event type.
    type Event: From<Event<Self>> + Into<<Self as frame_system::Trait>::Event>;
    /// The overarching dispatch call type.
    type Call: From<Call<Self>>;

    /// A configuration for base priority of unsigned transactions.
    ///
    /// This is exposed so that it can be tuned for particular runtime, when
    /// multiple pallets send unsigned transactions.
    type UnsignedPriority: Get<TransactionPriority>;

    /// The identifier type for an offchain worker.
    type AuthorityId: Parameter + Default + RuntimeAppPublic + Ord;
    // AppCrypto<Self::Public, Self::Signature>;
    // type TestAuthorityId: AppCrypto<Self::Public, Self::Signature>;
}

// #[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug)]
// pub struct Payload<Public> {
//     number: u64,
//     public: Public
// }
//
// impl <T: SigningTypes> SignedPayload<T> for Payload<T::Public> {
//     fn public(&self) -> T::Public {
//         self.public.clone()
//     }
// }

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

        fn offchain_worker(block_number: T::BlockNumber) {
            // Consider setting the frequency of requesting btc data based on the `block_number`
            debug::info!("ChainX Bitcoin Offchain Worker, ChainX Block #{:?}", block_number);

            let best_index = XGatewayBitcoin::<T>::best_index().height;
            let network = XGatewayBitcoin::<T>::network_id();

            let next_height = best_index + 1;
            let btc_block_hash = match Self::fetch_block_hash(next_height, network) {
                Ok(Some(hash)) => {
                    debug::info!("₿ Block #{} hash: {}", next_height, hash);
                    hash
                }
                Ok(None) => {
                    debug::warn!("₿ Block #{} has not been generated yet", next_height);
                    return;
                }
                Err(err) => {
                    debug::warn!("₿ {:?}", err);
                    return;
                }
            };

            let btc_block = match Self::fetch_block(&btc_block_hash[..], network) {
                Ok(block) => {
                    debug::info!("₿ Block {}", hash_rev(block.hash()));
                    block
                }
                Err(err) => {
                    debug::warn!("₿ {:?}", err);
                    return;
                }
            };

            // let key = Self::keys();
            // debug::info!("AuthorityId: {:?}", key);
            //
            // let signer = Signer::<T, T::TestAuthorityId>::any_account();
            // let results = signer.send_signed_transaction(|_account|{
            //     Call::push_header(block_number)
            // });

            let new_header_found = true;
            if new_header_found {
                let call = Call::push_header(block_number);
                debug::info!("₿ Submitting unsigned transaction: {:?}", call);
                if let Err(e) = SubmitTransaction::<T, Call<T>>::submit_unsigned_transaction(call.into()) {
                    debug::error!("Failed to submit unsigned transaction: {:?}", e);
                }
            }
        }

        #[weight = 0]
        fn push_header(origin, block_number: T::BlockNumber) {
            ensure_none(origin)?;
            debug::info!("--------------- push header from OCW");
            let best_index = XGatewayBitcoin::<T>::best_index().height;
            let network = XGatewayBitcoin::<T>::network_id();

            let next_height = best_index + 1;

            if let Ok(Some(hash)) = Self::fetch_block_hash(next_height, network) {
                debug::info!("₿ Block #{} hash: {}", next_height, hash);
                if let Ok(block) = Self::fetch_block(&hash[..], network) {
                    debug::info!("₿ Block {}", hash_rev(block.hash()));
                    let btc_header = block.header;
                    let header = serialize(&btc_header).take();
                    XGatewayBitcoin::<T>::test_push_header(header);
                }
            }
        }

        #[weight = 0]
        fn push_transaction(origin, block_number: T::BlockNumber) {
            let who = ensure_signed(origin)?;
            debug::info!("push transaction from OCW");
        }
    }
}

impl<T: Trait> Module<T> {
    fn initialize_keys(keys: &[T::AuthorityId]) {
        if !keys.is_empty() {
            assert!(Keys::<T>::get().is_empty(), "Keys are already initialized!");
            Keys::<T>::put(keys);
            // debug::info!("Keys are already initialized! Keys: {:?}", keys);
        }
    }
}

/// Most of the functions are moved outside of the `decl_module!` macro.
///
/// This greatly helps with error messages, as the ones inside the macro
/// can sometimes be hard to debug.
impl<T: Trait> Module<T> {
    // Submit XBTC deposit/withdraw transaction to the ChainX
    fn push_xbtc_transaction(confirmed_block: &BtcBlock, network: BtcNetwork) {
        let mut needed = Vec::new();
        let mut tx_hashes = Vec::with_capacity(confirmed_block.transactions.len());
        let mut tx_matches = Vec::with_capacity(confirmed_block.transactions.len());

        for tx in &confirmed_block.transactions {
            // Prepare for constructing partial merkle tree
            tx_hashes.push(tx.hash());
            if tx.is_coinbase() {
                tx_matches.push(false);
                continue;
            }
            let outpoint = tx.inputs[0].previous_output;
            let prev_tx_hash = hex::encode(hash_rev(outpoint.txid));
            let prev_tx = Self::fetch_transaction(&prev_tx_hash[..], network).unwrap();

            // Detect X-BTC transaction type
            // Withdrawal: must have a previous transaction
            // Deposit: don't require previous transaction generally,
            //          but in special cases, a previous transaction needs to be submitted.
            let btc_min_deposit = XGatewayBitcoin::<T>::btc_min_deposit();
            let current_trustee_pair = trustee::get_current_trustee_address_pair::<T>().unwrap();
            let last_trustee_pair = trustee::get_last_trustee_address_pair::<T>().unwrap();
            let btc_tx_detector = BtcTxTypeDetector::new(
                network,
                btc_min_deposit,
                current_trustee_pair,
                Some(last_trustee_pair),
            );

            match btc_tx_detector.detect_transaction_type(
                &tx,
                Some(&prev_tx),
                OpReturnExtractor::extract_account,
            ) {
                BtcTxMetaType::Withdrawal => {
                    debug::info!(
                        "X-BTC Withdrawal (PrevTx: {:?}, Tx: {:?})",
                        hash_rev(prev_tx.hash()),
                        hash_rev(tx.hash())
                    );
                    tx_matches.push(true);
                    needed.push((tx.clone(), Some(prev_tx)));
                }
                BtcTxMetaType::Deposit(BtcDepositInfo {
                    deposit_value,
                    op_return,
                    input_addr,
                }) => {
                    debug::info!(
                        "X-BTC Deposit [{}] (Tx: {:?})",
                        deposit_value,
                        hash_rev(tx.hash())
                    );
                    tx_matches.push(true);
                    match (input_addr, op_return) {
                        (_, Some((account, _))) => {
                            if Self::pending_deposits(account).is_empty() {
                                needed.push((tx.clone(), None));
                            } else {
                                needed.push((tx.clone(), Some(prev_tx)));
                            }
                        }
                        (Some(_), None) => needed.push((tx.clone(), Some(prev_tx))),
                        (None, None) => {
                            debug::warn!(
                                "[Service|push_xbtc_transaction] parsing prev_tx or op_return error, tx {:?}",
                                hash_rev(tx.hash())
                            );
                            needed.push((tx.clone(), Some(prev_tx)));
                        }
                    }
                }
                BtcTxMetaType::HotAndCold
                | BtcTxMetaType::TrusteeTransition
                | BtcTxMetaType::Irrelevance => tx_matches.push(false),
            }
        }

        if !needed.is_empty() {
            debug::info!(
                "[Service|push_xbtc_transaction] Generate partial merkle tree from the Confirmed Block {:?}",
                hash_rev(confirmed_block.hash())
            );

            // Construct partial merkle tree
            // We can never have zero txs in a merkle block, we always need the coinbase tx.
            let merkle_proof = PartialMerkleTree::from_txids(&tx_hashes, &tx_matches);

            // Push xbtc relay (withdraw/deposit) transaction
            for (tx, prev_tx) in needed {
                let relayed_info = BtcRelayedTxInfo {
                    block_hash: confirmed_block.hash(),
                    merkle_proof: merkle_proof.clone(),
                };
                let tx = serialize(&tx).take();
                let prev_tx = prev_tx.unwrap();
                let prev_tx = serialize(&prev_tx).take();
                XGatewayBitcoin::<T>::push_transaction(
                    RawOrigin::Root.into(),
                    tx,
                    relayed_info,
                    Some(prev_tx),
                )
                .unwrap();
            }
        } else {
            debug::info!(
                "[Service|push_xbtc_transaction] No X-BTC Deposit/Withdraw Transactions in th Confirmed Block {:?}",
                hash_rev(confirmed_block.hash())
            );
        }
    }
    // help use AccountId
    fn pending_deposits<P: AsRef<[u8]>>(btc_address: P) -> Vec<BtcDepositCache> {
        let btc_address = btc_address.as_ref();
        let deposit_cache: Vec<BtcDepositCache> =
            XGatewayBitcoin::<T>::pending_deposits(btc_address);
        deposit_cache
    }
    // push new btc block header to chain
    fn push_next_header(current_height: u32, next_height: u32, network: BtcNetwork) {
        if let Ok(Some(hash)) = Self::fetch_block_hash(next_height, network) {
            debug::info!("₿ Block #{} Hash: {:?}", next_height, hash);
            if let Ok(block) = Self::fetch_block(&hash[..], network) {
                debug::info!("₿ Block {}", hash_rev(block.hash()));
                let btc_header = block.header;
                if XGatewayBitcoin::<T>::block_hash_for(current_height)
                    .contains(&btc_header.previous_header_hash)
                {
                    let header = serialize(&btc_header).take();
                    match XGatewayBitcoin::<T>::push_header(RawOrigin::Root.into(), header)
                    {
                        Ok(yes) => {
                            debug::info!("Push header Success {:?}", yes);
                        }
                        Err(err) => {
                            debug::warn!("Push Header Error: {:?}", err);
                        }
                    }
                } else {
                    debug::warn!("Current block #{} may be a fork block", current_height);
                }
            }
        }
    }
    // get withdrawal proposal from chain and broadcast raw transaction
    fn get_withdrawal_proposal_broadcast(network: BtcNetwork) -> Result<Option<String>, ()> {
        if let Some(withdrawal_proposal) = XGatewayBitcoin::<T>::withdrawal_proposal() {
            if withdrawal_proposal.sig_state == VoteResult::Finish {
                let tx = serialize(&withdrawal_proposal.tx).take();
                let hex_tx = hex::encode(&tx);
                debug::info!("send_raw_transaction| Btc Tx Hex: {}", hex_tx);
                match Self::send_raw_transaction(hex_tx, network) {
                    Ok(hash) => {
                        debug::info!("send_raw_transaction| Transaction Hash: {:?}", hash);
                        return Ok(Some(hash));
                    }
                    Err(err) => {
                        debug::warn!("send_raw_transaction| Error {:?}", err);
                    }
                }
            }
        }
        Ok(None)
    }

    fn get<U: AsRef<str>>(url: U) -> Result<Vec<u8>, Error<T>> {
        // We want to keep the offchain worker execution time reasonable, so we set a hard-coded
        // deadline to 2s to complete the external call.
        // You can also wait indefinitely for the response, however you may still get a timeout
        // coming from the host machine.
        let deadline = sp_io::offchain::timestamp().add(Duration::from_millis(2_000));

        // Initiate an external HTTP GET request.
        // This is using high-level wrappers from `sp_runtime`, for the low-level calls that
        // you can find in `sp_io`. The API is trying to be similar to `reqwest`, but
        // since we are running in a custom WASM execution environment we can't simply
        // import the library here.
        // We set the deadline for sending of the request, note that awaiting response can
        // have a separate deadline. Next we send the request, before that it's also possible
        // to alter request headers or stream body content in case of non-GET requests.
        let pending = http::Request::get(url.as_ref())
            .deadline(deadline)
            .send()
            .map_err(|err| Error::<T>::from(err))?;

        // The request is already being processed by the host, we are free to do anything
        // else in the worker (we can send multiple concurrent requests too).
        // At some point however we probably want to check the response though,
        // so we can block current thread and wait for it to finish.
        // Note that since the request is being driven by the host, we don't have to wait
        // for the request to have it complete, we will just not read the response.
        let response = pending
            .try_wait(deadline)
            .map_err(|_| Error::<T>::HttpDeadlineReached)??;

        // Let's check the status code before we proceed to reading the response.
        if response.code != 200 {
            debug::warn!("Unexpected status code: {}", response.code);
            return Err(Error::<T>::HttpUnknown);
        }

        // Next we want to fully read the response body and collect it to a vector of bytes.
        // Note that the return object allows you to read the body in chunks as well
        // with a way to control the deadline.
        let resp_body = response.body().collect::<Vec<u8>>();
        Ok(resp_body)
    }

    fn post<B, I>(url: &str, req_body: B) -> Result<Vec<u8>, Error<T>>
    where
        B: Default + IntoIterator<Item = I>,
        I: AsRef<[u8]>,
    {
        // We want to keep the offchain worker execution time reasonable, so we set a hard-coded
        // deadline to 2s to complete the external call.
        // You can also wait indefinitely for the response, however you may still get a timeout
        // coming from the host machine.
        let deadline = sp_io::offchain::timestamp().add(Duration::from_millis(2_000));

        // Initiate an external HTTP POST request.
        // This is using high-level wrappers from `sp_runtime`, for the low-level calls that
        // you can find in `sp_io`. The API is trying to be similar to `reqwest`, but
        // since we are running in a custom WASM execution environment we can't simply
        // import the library here.
        // We set the deadline for sending of the request, note that awaiting response can
        // have a separate deadline. Next we send the request, before that it's also possible
        // to alter request headers or stream body content in case of non-GET requests.
        let pending = http::Request::post(url, req_body)
            .deadline(deadline)
            .send()
            .map_err(|err| Error::<T>::from(err))?;

        // The request is already being processed by the host, we are free to do anything
        // else in the worker (we can send multiple concurrent requests too).
        // At some point however we probably want to check the response though,
        // so we can block current thread and wait for it to finish.
        // Note that since the request is being driven by the host, we don't have to wait
        // for the request to have it complete, we will just not read the response.
        let response = pending
            .try_wait(deadline)
            .map_err(|_| Error::<T>::HttpDeadlineReached)??;

        // Let's check the status code before we proceed to reading the response.
        if response.code != 200 {
            debug::warn!("Unexpected status code: {}", response.code);
            return Err(Error::<T>::HttpUnknown);
        }

        // Next we want to fully read the response body and collect it to a vector of bytes.
        // Note that the return object allows you to read the body in chunks as well
        // with a way to control the deadline.
        let resp_body = response.body().collect::<Vec<u8>>();
        Ok(resp_body)
    }

    fn fetch_block_hash(height: u32, network: BtcNetwork) -> Result<Option<String>, Error<T>> {
        let url = match network {
            BtcNetwork::Mainnet => format!("https://blockstream.info/api/block-height/{}", height),
            BtcNetwork::Testnet => format!(
                "https://blockstream.info/testnet/api/block-height/{}",
                height
            ),
        };

        let resp_body = Self::get(url)?;
        let resp_body = str::from_utf8(&resp_body).map_err(|_| {
            debug::warn!("No UTF8 body");
            Error::<T>::HttpBodyNotUTF8
        })?;

        const RESP_BLOCK_NOT_FOUND: &str = "Block not found";
        if resp_body == RESP_BLOCK_NOT_FOUND {
            debug::info!("₿ Block #{} not found", height);
            Ok(None)
        } else {
            let hash: String = resp_body.into();
            debug::info!("₿ Block #{} hash: {:?}", height, hash);
            Ok(Some(hash))
        }
    }

    fn fetch_block(hash: &str, network: BtcNetwork) -> Result<BtcBlock, Error<T>> {
        let url = match network {
            BtcNetwork::Mainnet => format!("https://blockstream.info/api/block/{}/raw", hash),
            BtcNetwork::Testnet => {
                format!("https://blockstream.info/testnet/api/block/{}/raw", hash)
            }
        };
        let body = Self::get(url)?;
        let block = deserialize::<_, BtcBlock>(Reader::new(&body))
            .map_err(|_| Error::<T>::BtcSserializationError)?;

        debug::info!("₿ Block {}", hash_rev(block.hash()));
        Ok(block)
    }

    fn fetch_transaction(hash: &str, network: BtcNetwork) -> Result<BtcTransaction, Error<T>> {
        let url = match network {
            BtcNetwork::Mainnet => format!("https://blockstream.info/api/tx/{}/hex", hash),
            BtcNetwork::Testnet => format!("https://blockstream.info/testnet/api/tx/{}/hex", hash),
        };
        let body = Self::get(url)?;
        let transaction = deserialize::<_, BtcTransaction>(Reader::new(&body))
            .map_err(|_| Error::<T>::BtcSserializationError)?;
        debug::info!("₿ Transaction {}", hash_rev(transaction.hash()));
        Ok(transaction)
    }

    fn send_raw_transaction<TX: AsRef<[u8]>>(
        hex_tx: TX,
        network: BtcNetwork,
    ) -> Result<String, Error<T>> {
        let url = match network {
            BtcNetwork::Mainnet => "https://blockstream.info/api/tx",
            BtcNetwork::Testnet => "https://blockstream.info/testnet/api/tx",
        };
        let resp_body = Self::post(url, vec![hex_tx.as_ref()])?;
        let resp_body = str::from_utf8(&resp_body).map_err(|_| {
            debug::warn!("No UTF8 body");
            Error::<T>::HttpBodyNotUTF8
        })?;

        if resp_body.len() == 2 * BtcHash::len_bytes() {
            let hash: String = resp_body.into();
            debug::info!(
                "₿ Send Transaction successfully, Hash: {}, HexTx: {}",
                hash,
                hex::encode(hex_tx.as_ref())
            );
            Ok(hash)
        } else if resp_body.starts_with(SEND_RAW_TX_ERR_PREFIX) {
            if let Some(err) = Self::parse_send_raw_tx_error(resp_body) {
                debug::info!(
                    "₿ Send Transaction error: (code: {}, msg: {}), HexTx: {}",
                    err.code,
                    err.message,
                    hex::encode(hex_tx.as_ref())
                );
            } else {
                debug::info!(
                    "₿ Send Transaction unknown error, HexTx: {}",
                    hex::encode(hex_tx.as_ref())
                );
            }
            Err(Error::<T>::BtcSendRawTxError)
        } else {
            debug::info!(
                "₿ Send Transaction unknown error, HexTx: {}",
                hex::encode(hex_tx.as_ref())
            );
            Err(Error::<T>::BtcSendRawTxError)
        }
    }

    fn parse_send_raw_tx_error(resp_body: &str) -> Option<SendRawTxError> {
        use lite_json::JsonValue;
        let rest_resp = resp_body.trim_start_matches(SEND_RAW_TX_ERR_PREFIX);
        let value = lite_json::parse_json(rest_resp).ok();
        value.and_then(|v| match v {
            JsonValue::Object(obj) => {
                let code = obj
                    .iter()
                    .find(|(k, _)| k == &['c', 'o', 'd', 'e'])
                    .map(|(_, code)| code);
                let message = obj
                    .iter()
                    .find(|(k, _)| k == &['m', 'e', 's', 's', 'a', 'g', 'e'])
                    .map(|(_, msg)| msg);
                match (code, message) {
                    (Some(JsonValue::Number(code)), Some(JsonValue::String(msg))) => {
                        Some(SendRawTxError {
                            code: code.integer,
                            message: msg.into_iter().collect(),
                        })
                    }
                    _ => None,
                }
            }
            _ => None,
        })
    }
}

const SEND_RAW_TX_ERR_PREFIX: &str = "sendrawtransaction RPC error: ";
struct SendRawTxError {
    code: i64,
    message: String,
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
        if let Call::push_header(block_number) = call {
            ValidTransaction::with_tag_prefix("XGatewayBitcoinOffchain")
                .priority(T::UnsignedPriority::get())
                .and_provides(block_number) // TODO: a tag is required, otherwise the transactions will not be pruned.
                // .and_provides((current_session, authority_id)) provide a tag?
                .longevity(1u64) // FIXME a proper longevity
                .propagate(true)
                .build()
        } else {
            InvalidTransaction::Call.into()
        }
    }
}
