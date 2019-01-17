extern crate runtime_api;

use std::collections::BTreeMap;
use std::iter::FromIterator;
use std::sync::Arc;

use btc_chain::Transaction as BTCTransaction;
use codec::{Decode, Encode};
use jsonrpc_macros::Trailing;
use serde::Serialize;

use client::runtime_api::Core as CoreAPI;
use client::{self, runtime_api::Metadata, Client};
use keys::Address;
use script::Script;
use state_machine::Backend;

use primitives::storage::{StorageData, StorageKey};
use primitives::{Blake2Hasher, H256};
use runtime_primitives::generic::{BlockId, SignedBlock};
use runtime_primitives::traits::{As, Block as BlockT, Header, NumberFor, ProvideRuntimeApi, Zero};

use srml_support::storage::{StorageMap, StorageValue};

use chainx_primitives::{AccountId, Balance, BlockNumber, Timestamp};
use chainx_runtime::Runtime;

use xaccounts::{self, CertImmutableProps, IntentionImmutableProps, IntentionProps};
use xassets::{self, assetdef::ChainT, Asset, AssetType, Chain, Token};
use xbitcoin::{
    self, from, AccountMap, BestIndex, BlockHeaderFor, BlockHeaderInfo, IrrBlock, TrusteeAddress,
    TxFor, TxInfo,
};
use xrecords::{self, Application};
use xspot::def::{OrderPair, OrderPairID, ID};
use xspot::{HandicapT, OrderT};
use xstaking::{self, IntentionProfs, NominationRecord};
use xsupport::storage::btree_map::CodecBTreeMap;
use xtokens::{self, DepositVoteWeight, PseduIntentionVoteWeight};

use self::runtime_api::{xassets_api::XAssetsApi, xmining_api::XMiningApi};

mod error;
mod impl_rpc;
mod types;

use self::error::Result;
use self::types::{
    ApplicationWrapper, AssetInfo, CertInfo, IntentionInfo, PageData, PairInfo, PseduIntentionInfo,
    PseduNominationRecord, QuotationsList, TotalAssetInfo, WithdrawalState,
};
use chainx::error::ErrorKind::{OrderPairIDErr, PageIndexErr, PageSizeErr, QuotationssPieceErr};

const MAX_PAGE_SIZE: u32 = 100;

build_rpc_trait! {
    /// ChainX API
    pub trait ChainXApi<Number, AccountId, Balance, BlockNumber> where SignedBlock: Serialize, {

        /// Returns the block of a storage entry at a block's Number.
        #[rpc(name = "chainx_getBlockByNumber")]
        fn block_info(&self, Trailing<Number>) -> Result<Option<SignedBlock>>;

        #[rpc(name = "chainx_getCertByAccount")]
        fn cert(&self, AccountId) -> Result<Option<Vec<CertInfo>>>;

        #[rpc(name = "chainx_getAssetsByAccount")]
        fn assets_of(&self, AccountId, u32, u32) -> Result<Option<PageData<AssetInfo>>>;

        #[rpc(name = "chainx_getAssets")]
        fn assets(&self, u32, u32) -> Result<Option<PageData<TotalAssetInfo>>>;

        #[rpc(name = "chainx_getVerifyAddress")]
        fn verify_addr(&self, xassets::Token, xrecords::AddrStr, xassets::Memo) -> Result<Option<Vec<u8>>>;

        #[rpc(name = "chainx_getWithdrawalList")]
        fn withdrawal_list(&self, u32, u32) -> Result<Option<PageData<ApplicationWrapper>>>;

        #[rpc(name = "chainx_getWithdrawalListByAccount")]
        fn withdrawal_list_of(&self, AccountId, u32, u32) -> Result<Option<PageData<ApplicationWrapper>>>;

        #[rpc(name = "chainx_getNominationRecords")]
        fn nomination_records(&self, AccountId) -> Result<Option<Vec<(AccountId, NominationRecord<Balance, BlockNumber>)>>>;

        #[rpc(name = "chainx_getIntentions")]
        fn intentions(&self) -> Result<Option<Vec<IntentionInfo>>>;

        #[rpc(name = "chainx_getPseduIntentions")]
        fn psedu_intentions(&self) -> Result<Option<Vec<PseduIntentionInfo>>>;

        #[rpc(name = "chainx_getPseduNominationRecords")]
        fn psedu_nomination_records(&self, AccountId) -> Result<Option<Vec<PseduNominationRecord>>>;

        #[rpc(name = "chainx_getOrderPairs")]
        fn order_pairs(&self) -> Result<Option<Vec<(PairInfo)>>>;

        #[rpc(name = "chainx_getQuotations")]
        fn quotationss(&self, OrderPairID, u32) -> Result<Option<QuotationsList>>;

        #[rpc(name = "chainx_getOrders")]
        fn orders(&self, AccountId, u32, u32) -> Result<Option<PageData<OrderT<Runtime>>>>;

        #[rpc(name = "chainx_getDepositRecords")]
        fn deposit_records(&self, AccountId) -> Result<Option<Vec<(u64, String, BlockNumber, String, Balance, String)>>>;
    }
}

/// ChainX API
pub struct ChainX<B, E, Block, RA>
where
    B: client::backend::Backend<Block, Blake2Hasher>,
    E: client::CallExecutor<Block, Blake2Hasher> + Clone + Send + Sync,
    Block: BlockT<Hash = H256>,
    RA: CoreAPI<Block> + XAssetsApi<Block> + XMiningApi<Block>,
{
    client: Arc<Client<B, E, Block, RA>>,
}

impl<B, E, Block: BlockT, RA> ChainX<B, E, Block, RA>
where
    B: client::backend::Backend<Block, Blake2Hasher> + Send + Sync + 'static,
    E: client::CallExecutor<Block, Blake2Hasher> + Clone + Send + Sync,
    Block: BlockT<Hash = H256> + 'static,
    RA: Metadata<Block> + XAssetsApi<Block> + XMiningApi<Block>,
{
    /// Create new ChainX API RPC handler.
    pub fn new(client: Arc<Client<B, E, Block, RA>>) -> Self {
        Self { client }
    }

    fn to_storage_key(key: &[u8]) -> StorageKey {
        let hashed = primitives::twox_128(key).to_vec();
        StorageKey(hashed)
    }

    /// Get best state of the chain.
    fn best_number(&self) -> std::result::Result<BlockId<Block>, client::error::Error> {
        let best_hash = self.client.info()?.chain.best_hash;
        Ok(BlockId::Hash(best_hash))
    }

    fn best_state(
        &self,
    ) -> std::result::Result<
        <B as client::backend::Backend<Block, Blake2Hasher>>::State,
        client::error::Error,
    > {
        let state = self.client.state_at(&self.best_number()?)?;
        Ok(state)
    }

    fn timestamp(&self, number: BlockNumber) -> std::result::Result<Timestamp, error::Error> {
        let number = number.encode();
        let number: NumberFor<Block> = Decode::decode(&mut number.as_slice()).unwrap();

        let state = self.client.state_at(&BlockId::Number(number))?;

        let key = <timestamp::Now<Runtime>>::key();

        Ok(Self::pickout::<Timestamp>(&state, &key)?.unwrap())
    }

    /// Pick out specified data from storage given the state and key.
    fn pickout<ReturnValue: Decode>(
        state: &<B as client::backend::Backend<Block, Blake2Hasher>>::State,
        key: &[u8],
    ) -> std::result::Result<Option<ReturnValue>, error::Error> {
        Ok(state
            .storage(&Self::to_storage_key(key).0)
            .map_err(|e| error::Error::from_state(Box::new(e)))?
            .map(StorageData)
            .map(|s| Decode::decode(&mut s.0.as_slice()))
            .unwrap_or(None))
    }

    fn get_asset(
        state: &<B as client::backend::Backend<Block, Blake2Hasher>>::State,
        token: &Token,
    ) -> Result<Option<Asset>> {
        let key = <xassets::AssetInfo<Runtime>>::key_for(token.as_ref());
        match Self::pickout::<(Asset, bool, BlockNumber)>(&state, &key)? {
            Some((info, _, _)) => Ok(Some(info)),
            None => Ok(None),
        }
    }

    fn get_applications_with_state(
        state: &<B as client::backend::Backend<Block, Blake2Hasher>>::State,
        v: Vec<Application<AccountId, Balance, Timestamp>>,
    ) -> Result<Vec<ApplicationWrapper>> {
        // todo change to runtime?
        let mut handle = BTreeMap::<Chain, Vec<u32>>::new();
        // btc
        let key = xbitcoin::TxProposal::<Runtime>::key();
        let ids = match Self::pickout::<xbitcoin::CandidateTx>(&state, &key)? {
            Some(candidate_tx) => candidate_tx.outs,
            None => vec![],
        };
        handle.insert(Chain::Bitcoin, ids);

        let mut applications = vec![];
        for appl in v {
            let index = appl.id();
            let token = appl.token();

            let state = if let Some(info) = Self::get_asset(state, &token)? {
                match handle.get(&info.chain()) {
                    Some(list) => {
                        if list.contains(&index) {
                            WithdrawalState::Signing
                        } else {
                            WithdrawalState::Applying
                        }
                    }
                    None => WithdrawalState::Unknown,
                }
            } else {
                unreachable!("should not reach this branch, the token info must be exists");
            };
            applications.push(ApplicationWrapper::new(appl, state));
        }
        Ok(applications)
    }
}
