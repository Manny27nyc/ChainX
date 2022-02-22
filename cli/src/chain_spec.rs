// Copyright 2019-2020 ChainX Project Authors. Licensed under GPL-3.0.

#![allow(unused)]
use std::collections::BTreeMap;
use std::convert::TryInto;

use hex_literal::hex;
use serde::{Deserialize, Serialize};
use serde_json::json;

use sc_chain_spec::ChainSpecExtension;
use sc_service::config::TelemetryEndpoints;
use sc_service::{ChainType, Properties};

use sp_authority_discovery::AuthorityId as AuthorityDiscoveryId;
use sp_consensus_babe::AuthorityId as BabeId;
use sp_core::{crypto::UncheckedInto, sr25519, Pair, Public};
use sp_finality_grandpa::AuthorityId as GrandpaId;
use sp_runtime::traits::{IdentifyAccount, Verify};

use pallet_im_online::sr25519::AuthorityId as ImOnlineId;

use chainx_primitives::{AccountId, AssetId, Balance, ReferralId, Signature};
use chainx_runtime::constants::currency::DOLLARS;
use dev_runtime::constants::{currency::DOLLARS as DEV_DOLLARS, time::DAYS as DEV_DAYS};
use xp_assets_registrar::Chain;
use xp_protocol::{NetworkType, PCX, PCX_DECIMALS, X_BTC};
use xpallet_gateway_bitcoin::{BtcParams, BtcTxVerifier};
use xpallet_gateway_common::types::TrusteeInfoConfig;

use crate::genesis::assets::{genesis_assets, init_assets, pcx, AssetParams};
use crate::genesis::bitcoin::{btc_genesis_params, BtcGenesisParams, BtcTrusteeParams};

use chainx_runtime as chainx;
use dev_runtime as dev;
use malan_runtime as malan;

// Note this is the URL for the telemetry server
#[allow(unused)]
const POLKADOT_TELEMETRY_URL: &str = "wss://telemetry.polkadot.io/submit/";
#[allow(unused)]
const CHAINX_TELEMETRY_URL: &str = "wss://telemetry.chainx.org/submit/";

/// Node `ChainSpec` extensions.
///
/// Additional parameters for some Substrate core modules,
/// customizable from the chain spec.
#[derive(Default, Clone, Serialize, Deserialize, ChainSpecExtension)]
#[serde(rename_all = "camelCase")]
pub struct Extensions {
    /// Block numbers with known hashes.
    pub fork_blocks: sc_client_api::ForkBlocks<chainx_primitives::Block>,
    /// Known bad block hashes.
    pub bad_blocks: sc_client_api::BadBlocks<chainx_primitives::Block>,
}

/// The `ChainSpec` parameterised for the chainx mainnet runtime.
pub type ChainXChainSpec = sc_service::GenericChainSpec<chainx::GenesisConfig, Extensions>;
/// The `ChainSpec` parameterised for the chainx testnet runtime.
pub type DevChainSpec = sc_service::GenericChainSpec<dev::GenesisConfig, Extensions>;
/// The `ChainSpec` parameterised for the chainx development runtime.
pub type MalanChainSpec = sc_service::GenericChainSpec<malan::GenesisConfig, Extensions>;

type AccountPublic = <Signature as Verify>::Signer;

/// Helper function to generate a crypto pair from seed
pub fn get_from_seed<TPublic: Public>(seed: &str) -> <TPublic::Pair as Pair>::Public {
    TPublic::Pair::from_string(&format!("//{}", seed), None)
        .expect("static values are valid; qed")
        .public()
}

/// Helper function to generate an account ID from seed
pub fn get_account_id_from_seed<TPublic: Public>(seed: &str) -> AccountId
where
    AccountPublic: From<<TPublic::Pair as Pair>::Public>,
{
    AccountPublic::from(get_from_seed::<TPublic>(seed)).into_account()
}

type AuthorityKeysTuple = (
    (AccountId, ReferralId), // (Staking ValidatorId, ReferralId)
    BabeId,
    GrandpaId,
    ImOnlineId,
    AuthorityDiscoveryId,
);

/// Helper function to generate an authority key for babe
pub fn authority_keys_from_seed(seed: &str) -> AuthorityKeysTuple {
    (
        (
            get_account_id_from_seed::<sr25519::Public>(seed),
            seed.as_bytes().to_vec(),
        ),
        get_from_seed::<BabeId>(seed),
        get_from_seed::<GrandpaId>(seed),
        get_from_seed::<ImOnlineId>(seed),
        get_from_seed::<AuthorityDiscoveryId>(seed),
    )
}

#[inline]
fn balance(input: Balance, decimals: u8) -> Balance {
    input * 10_u128.pow(decimals as u32)
}

/// A small macro for generating the info of PCX endowed accounts.
macro_rules! endowed_gen {
    ( $( ($seed:expr, $value:expr), )+ ) => {
        {
            let mut endowed = BTreeMap::new();
            let pcx_id = pcx().0;
            let endowed_info = vec![
                $((get_account_id_from_seed::<sr25519::Public>($seed), balance($value, PCX_DECIMALS)),)+
            ];
            endowed.insert(pcx_id, endowed_info);
            endowed
        }
    }
}

/// Helper function to generate the network properties.
fn as_properties(network: NetworkType) -> Properties {
    json!({
        "ss58Format": network.ss58_addr_format_id(),
        "network": network,
        "tokenDecimals": PCX_DECIMALS,
        "tokenSymbol": "PCX"
    })
    .as_object()
    .expect("network properties generation can not fail; qed")
    .to_owned()
}

pub fn development_config() -> Result<DevChainSpec, String> {
    let wasm_binary =
        dev::WASM_BINARY.ok_or_else(|| "Development wasm binary not available".to_string())?;

    let endowed_balance = 50 * DEV_DOLLARS;
    let constructor = move || {
        build_genesis(
            wasm_binary,
            vec![authority_keys_from_seed("Alice")],
            get_account_id_from_seed::<sr25519::Public>("Alice"),
            genesis_assets(),
            endowed_gen![
                ("Alice", endowed_balance),
                ("Bob", endowed_balance),
                ("Alice//stash", endowed_balance),
                ("Bob//stash", endowed_balance),
            ],
            btc_genesis_params(include_str!("res/btc_genesis_params_testnet.json")),
            crate::genesis::bitcoin::local_testnet_trustees(),
        )
    };
    Ok(DevChainSpec::from_genesis(
        "Development",
        "dev",
        ChainType::Development,
        constructor,
        vec![],
        None,
        Some("chainx-dev"),
        Some(as_properties(NetworkType::Testnet)),
        Default::default(),
    ))
}

#[cfg(feature = "runtime-benchmarks")]
pub fn benchmarks_config() -> Result<DevChainSpec, String> {
    let wasm_binary =
        dev::WASM_BINARY.ok_or_else(|| "Development wasm binary not available".to_string())?;

    let endowed_balance = 50 * DEV_DOLLARS;
    let constructor = move || {
        build_genesis(
            wasm_binary,
            vec![authority_keys_from_seed("Alice")],
            get_account_id_from_seed::<sr25519::Public>("Alice"),
            genesis_assets(),
            endowed_gen![
                ("Alice", endowed_balance),
                ("Bob", endowed_balance),
                ("Alice//stash", endowed_balance),
                ("Bob//stash", endowed_balance),
            ],
            btc_genesis_params(include_str!("res/btc_genesis_params_benchmarks.json")),
            crate::genesis::bitcoin::benchmarks_trustees(),
        )
    };
    Ok(DevChainSpec::from_genesis(
        "Benchmarks",
        "dev",
        ChainType::Development,
        constructor,
        vec![],
        None,
        Some("chainx-dev"),
        Some(as_properties(NetworkType::Testnet)),
        Default::default(),
    ))
}

pub fn local_testnet_config() -> Result<DevChainSpec, String> {
    let wasm_binary =
        dev::WASM_BINARY.ok_or_else(|| "Development wasm binary not available".to_string())?;

    let endowed_balance = 50 * DEV_DOLLARS;
    let constructor = move || {
        build_genesis(
            wasm_binary,
            vec![
                authority_keys_from_seed("Alice"),
                authority_keys_from_seed("Bob"),
            ],
            get_account_id_from_seed::<sr25519::Public>("Alice"),
            genesis_assets(),
            endowed_gen![
                ("Alice", endowed_balance),
                ("Bob", endowed_balance),
                ("Charlie", endowed_balance),
                ("Dave", endowed_balance),
                ("Eve", endowed_balance),
                ("Ferdie", endowed_balance),
                ("Alice//stash", endowed_balance),
                ("Bob//stash", endowed_balance),
                ("Charlie//stash", endowed_balance),
                ("Dave//stash", endowed_balance),
                ("Eve//stash", endowed_balance),
                ("Ferdie//stash", endowed_balance),
            ],
            btc_genesis_params(include_str!("res/btc_genesis_params_testnet.json")),
            crate::genesis::bitcoin::local_testnet_trustees(),
        )
    };
    Ok(DevChainSpec::from_genesis(
        "Local Testnet",
        "local_testnet",
        ChainType::Local,
        constructor,
        vec![],
        None,
        Some("chainx-local-testnet"),
        Some(as_properties(NetworkType::Testnet)),
        Default::default(),
    ))
}

pub fn mainnet_config() -> Result<ChainXChainSpec, String> {
    ChainXChainSpec::from_json_bytes(&include_bytes!("./res/chainx_regenesis.json")[..])
    // build_mainnet_config()
}

pub fn malan_config() -> Result<MalanChainSpec, String> {
    MalanChainSpec::from_json_bytes(&include_bytes!("./res/malan.json")[..])
}

fn dev_session_keys(
    babe: BabeId,
    grandpa: GrandpaId,
    im_online: ImOnlineId,
    authority_discovery: AuthorityDiscoveryId,
) -> dev::SessionKeys {
    dev::SessionKeys {
        grandpa,
        babe,
        im_online,
        authority_discovery,
    }
}

fn build_genesis(
    wasm_binary: &[u8],
    initial_authorities: Vec<AuthorityKeysTuple>,
    root_key: AccountId,
    assets: Vec<AssetParams>,
    endowed: BTreeMap<AssetId, Vec<(AccountId, Balance)>>,
    bitcoin: BtcGenesisParams,
    trustees: Vec<(Chain, TrusteeInfoConfig, Vec<BtcTrusteeParams>)>,
) -> dev::GenesisConfig {
    const ENDOWMENT: Balance = 10_000_000 * DEV_DOLLARS;
    const STASH: Balance = 100 * DEV_DOLLARS;
    let (assets, assets_restrictions) = init_assets(assets);

    let endowed_accounts = endowed
        .get(&PCX)
        .expect("PCX endowed; qed")
        .iter()
        .cloned()
        .map(|(k, _)| k)
        .collect::<Vec<_>>();

    let num_endowed_accounts = endowed_accounts.len();

    let mut total_endowed = Balance::default();
    let balances = endowed
        .get(&PCX)
        .expect("PCX endowed; qed")
        .iter()
        .cloned()
        .map(|(k, _)| {
            total_endowed += ENDOWMENT;
            (k, ENDOWMENT)
        })
        .collect::<Vec<_>>();

    // The value of STASH balance will be reserved per phragmen member.
    let phragmen_members = endowed_accounts
        .iter()
        .take((num_endowed_accounts + 1) / 2)
        .cloned()
        .map(|member| (member, STASH))
        .collect();

    let tech_comm_members = endowed_accounts
        .iter()
        .take((num_endowed_accounts + 1) / 2)
        .cloned()
        .collect::<Vec<_>>();

    // PCX only reserves the native asset id in assets module,
    // the actual native fund management is handled by pallet_balances.
    let mut assets_endowed = endowed;
    assets_endowed.remove(&PCX);

    let btc_genesis_trustees = trustees
        .iter()
        .find_map(|(chain, _, trustee_params)| {
            if *chain == Chain::Bitcoin {
                Some(
                    trustee_params
                        .iter()
                        .map(|i| (i.0).clone())
                        .collect::<Vec<_>>(),
                )
            } else {
                None
            }
        })
        .expect("bitcoin trustees generation can not fail; qed");

    dev::GenesisConfig {
        frame_system: Some(dev::SystemConfig {
            code: wasm_binary.to_vec(),
            changes_trie_config: Default::default(),
        }),
        pallet_babe: Some(Default::default()),
        pallet_grandpa: Some(dev::GrandpaConfig {
            authorities: vec![],
        }),
        pallet_collective_Instance1: Some(dev::CouncilConfig::default()),
        pallet_collective_Instance2: Some(dev::TechnicalCommitteeConfig {
            members: tech_comm_members,
            phantom: Default::default(),
        }),
        pallet_membership_Instance1: Some(Default::default()),
        pallet_democracy: Some(dev::DemocracyConfig::default()),
        pallet_treasury: Some(Default::default()),
        pallet_elections_phragmen: Some(dev::ElectionsConfig {
            members: phragmen_members,
        }),
        pallet_im_online: Some(dev::ImOnlineConfig { keys: vec![] }),
        pallet_authority_discovery: Some(dev::AuthorityDiscoveryConfig { keys: vec![] }),
        pallet_session: Some(dev::SessionConfig {
            keys: initial_authorities
                .iter()
                .map(|x| {
                    (
                        (x.0).0.clone(),
                        (x.0).0.clone(),
                        dev_session_keys(x.1.clone(), x.2.clone(), x.3.clone(), x.4.clone()),
                    )
                })
                .collect::<Vec<_>>(),
        }),
        pallet_balances: Some(dev::BalancesConfig { balances }),
        pallet_indices: Some(dev::IndicesConfig { indices: vec![] }),
        pallet_sudo: Some(dev::SudoConfig { key: root_key }),
        xpallet_system: Some(dev::XSystemConfig {
            network_props: NetworkType::Testnet,
        }),
        xpallet_assets_registrar: Some(dev::XAssetsRegistrarConfig { assets }),
        xpallet_assets: Some(dev::XAssetsConfig {
            assets_restrictions,
            endowed: assets_endowed,
        }),
        xpallet_gateway_common: Some(dev::XGatewayCommonConfig { trustees }),
        xpallet_gateway_bitcoin: Some(dev::XGatewayBitcoinConfig {
            genesis_trustees: btc_genesis_trustees,
            network_id: bitcoin.network,
            confirmation_number: bitcoin.confirmation_number,
            genesis_hash: bitcoin.hash(),
            genesis_info: (bitcoin.header(), bitcoin.height),
            params_info: BtcParams::new(
                486604799,            // max_bits
                2 * 60 * 60,          // block_max_future
                2 * 7 * 24 * 60 * 60, // target_timespan_seconds
                10 * 60,              // target_spacing_seconds
                4,                    // retargeting_factor
            ), // retargeting_factor
            btc_withdrawal_fee: 500000,
            max_withdrawal_count: 100,
            verifier: BtcTxVerifier::Recover,
        }),
        xpallet_mining_staking: Some(dev::XStakingConfig {
            validator_count: 50,
            sessions_per_era: 12,
            glob_dist_ratio: (12, 88), // (Treasury, X-type Asset and Staking) = (12, 88)
            mining_ratio: (10, 90),    // (Asset Mining, Staking) = (10, 90)
            minimum_penalty: 2 * DOLLARS,
            ..Default::default()
        }),
        xpallet_mining_asset: Some(dev::XMiningAssetConfig {
            claim_restrictions: vec![(X_BTC, (10, DEV_DAYS * 7))],
            mining_power_map: vec![(X_BTC, 400)],
        }),
        xpallet_dex_spot: Some(dev::XSpotConfig {
            trading_pairs: vec![(PCX, X_BTC, 9, 2, 100000, true)],
        }),
        xpallet_genesis_builder: Some(dev::XGenesisBuilderConfig {
            params: crate::genesis::genesis_builder_params(),
            initial_authorities: initial_authorities
                .iter()
                .map(|i| (i.0).1.clone())
                .collect(),
        }),
    }
}

macro_rules! bootnodes {
    ( $( $bootnode:expr, )* ) => {
        vec![
            $($bootnode.to_string().try_into().expect("The bootnode is invalid"),)*
        ]
    }
}

pub fn build_mainnet_config() -> Result<ChainXChainSpec, String> {
    let wasm_binary = chainx::WASM_BINARY.ok_or("ChainX wasm binary not available".to_string())?;

    let initial_authorities: Vec<AuthorityKeysTuple> = vec![
        (
            (
                // 5Eve9bjfmNg7ZcnFgH1GyVwHVRCCqpV8Bbnwo1KGSfmERybR
                hex!["7e8e412cd967337f43ab6fc85efb3578bf4013afadd73aa452560d1e3d16ef46"].into(),
                b"Validator1".to_vec(),
            ),
            // 5HJwGngjkLeA2Mk45tk65RKAuuXhZNB3XSr2bV1nEHDmppH1
            hex!["e807c3c53c6648caa42a42dbcaffa90f03ce450a8ea4b3eb84232a243a62e635"].unchecked_into(),
            // 5HGr3kBp92U5LFdku4PPTv8RDZuc5cvm8tduCfsPKqV12c2w
            hex!["e66faf76972028ce6fbe1c428287dc817b7bfdbfc35a7e669ef738c01ca606d4"].unchecked_into(),
            // 5CA9ima78mS2bfFNGV6Y468hXv5FvLcVY7nDja5vaV6qJgCn
            hex!["04274ad6dd072750e13a0166f38ad62e641e84e00b86d98729c8f549df053d4c"].unchecked_into(),
            // 5HbkSGCHcFfVei2B6TbC4ub4gAuc3wVUna4U6xmk61VdjDuR
            hex!["f4da750a3265f11ac9355897c69ab04967b3aba1c6b261b8b08099889d9c505e"].unchecked_into(),
        ),
        (
            (
                // 5DtVBqdvHek4b2UGZ1aEjXuFXAW28AmjcSczg6E95VSyFNZo
                hex!["50ad5d161012015c6da79220bbd64280597a82d6c2b1b44736c8a4061b68fe71"].into(),
                b"Validator2".to_vec(),
            ),
            // 5FAFJ7zUfAjsVbxut5iDqsfLqKRsafS5CoBEF1U2EbqFXVYy
            hex!["88eec28ba2fd30b7ecf4d6dc2680f3546ca5b9b9e39516906e7d08d69e508638"].unchecked_into(),
            // 5EpuyeyHJVVgJHuG5hiNtEv57FrTJdrKr6HpSUWnH2i6SpYi
            hex!["7a3010d8a9154092511bb0639b022ce2a351770c0d54d222fbd46cdd6976ab75"].unchecked_into(),
            // 5GQZgo9t61Ud1iQzKgvagNMAFB77AJyeEgGfVzpg1k3aDyJm
            hex!["c01656a93e24555a97f263c162bdceee3602b6a0f803a24a03a41e963c5f5a36"].unchecked_into(),
            // 5EyZoQqTmHStN95R5wunWrURRm56SNvW1eEBPknEBucnLn3J
            hex!["80c95a995dab0b679e88f77bd5a7dd5bbd77a0950fd9d05c0dee093642534213"].unchecked_into(),
        ),
        (
            (
                // 5CJzKBFaFbhs8yxw2JMMmYJvPA629KaZrv9thtFDKCXDdM8T
                hex!["0ae4d5c42b266087e8587a3392d7747da7529b63eb1f842cf93fe465cfcd906b"].into(),
                b"Validator3".to_vec(),
            ),
            // 5EUG3nVcFWHgsBeWoeGNHv6GRAMBezFJHgF99rJ94GMYapgE
            hex!["6a6f7a5e5faa08c3ee581911b7d2a84ca253230e33ef47138433e9fa2aa4c770"].unchecked_into(),
            // 5HCBSJbBCXesbf3GUwPg12p9Q7D2xTX4oHZ7YyirpnH9PxHG
            hex!["e2e0ba3598d1c8e13ba9d770be570ac9b68f4576decb00222a6c99d1b7ef7c43"].unchecked_into(),
            // 5Cyg14JuxXgJEv4ppBLLQi3EupjJJuedSLk6QmN7aYQHuqqD
            hex!["2865c2bf5a64e71f3728071a07a1d8c5aeaa4eea351cfeaa4cec50bedbc57b69"].unchecked_into(),
            // 5Ctj4gHx1akBCmqcEajGhuTr9mgCoYzJurvXDcjJQBDhRcnd
            hex!["249fd7bda8008fbe07a04c9fa0f6f472ab6d20ca25fe3a29300c940268849602"].unchecked_into(),
        ),
        (
            (
                // 5GGmFtYo7adsu7FA8XQ8MDWS3rd9fDFdDq15UAHvnRUbPdfV
                hex!["ba2353466beeb849baf48b2ce1c39963a0595bf307813ffb754d6dfe9b2f051d"].into(),
                b"Validator4".to_vec(),
            ),
            // 5DaVg8nNKnFuYbdKqV7dPRY9e5ZeQFKHpTAPARDEiWvGBXUp
            hex!["42f496aef58d729bfc3005ffb6563bdeadfd4927ac21018d0880e53749b31476"].unchecked_into(),
            // 5CVE9fHhEFHk7dMnFL4ZrDhzgfKwkpag5HY8NGhEGihrmyTb
            hex!["12b3dd6bf23097e98864e00ae9066d07a45a91f5b86f00499bef384ab87c9a6a"].unchecked_into(),
            // 5CVVQEDM7pvQ2vnVKVnEzPcbnuN8Wto3y2uN7sEszstQzQW1
            hex!["12e7347670d6e14f14257472588d10bd036d76cd92ee1dc52140f3994a26d503"].unchecked_into(),
            // 5FNioVZf9TY7GM5DyaaUMG8BmKNZcP7W2mhGyj6aJ6ZPNsDt
            hex!["92724cb4d784a4b301a7e215492607ac1fd4a0f764ef1cfa31fbbee2527cdc50"].unchecked_into(),
        ),
        (
            (
                // 5D4jhctYbidYPodVc8537YFmHvHf7wM5YY5YQeHQW67i9XBU
                hex!["2c4270db11ec6c552cf5e1c84e448a1714334cd4a01e6f44890ba7b58849173a"].into(),
                b"Validator5".to_vec(),
            ),
            // 5DXjRYiVdUdLMVaFrPcrvBm4pA8cQLcTAMjbdx2NotWequmD
            hex!["40d9224a89aaa08913c8f814c176319054b353b1107a2ed892847830893fc175"].unchecked_into(),
            // 5FiGe8gup2tsv9ZBhhqd2otmk3AgGn3QgWwBzogcEXRE1jaG
            hex!["a15b24032ef3d31acd9e93f1bb88301a9b3c85d6e8b49af882a94f8037deaa43"].unchecked_into(),
            // 5HGdAoqeUujibSsNMQsDwv79vGwZ9bDJLyEnjzsw62As2Sfw
            hex!["e644555bfa2ceb6d60ab92f874f2b4b2927723669def2ad20c14cf2810191713"].unchecked_into(),
            // 5ELXPKoCkgaVHz9dsU8PGkJfX8YcXqiaRVvPsDgUmFMg1jRX
            hex!["648924b2638eb592663d634568b7e845aec79a247cb53e19fc621120b1f4c932"].unchecked_into(),
        ),
    ];
    let constructor = move || {
        mainnet_genesis(
            &wasm_binary[..],
            initial_authorities.clone(),
            genesis_assets(),
            btc_genesis_params(include_str!("res/btc_genesis_params_mainnet.json")),
            crate::genesis::bitcoin::mainnet_trustees(),
        )
    };

    // TODO: make sure the bootnodes
    // let bootnodes = bootnodes![
    // "/dns/p2p.1.chainx.org/tcp/20222/p2p/12D3KooWMMGD6eyLDgoTPnmGrawn9gkjtsZGLACJXqVCUbe6R6bD",
    // "/dns/p2p.2.chainx.org/tcp/20222/p2p/12D3KooWC1tFLBFVw47S2nfD7Nzhg5hBMUvsnz4nqpr82zfTYWaH",
    // "/dns/p2p.3.chainx.org/tcp/20222/p2p/12D3KooWPthFY8xDDyM5X9PWZwNfioqP5EShiTKyVv5899H22WBT",
    // ];

    let bootnodes = Default::default();

    Ok(ChainXChainSpec::from_genesis(
        "ChainX",
        "chainx",
        ChainType::Live,
        constructor,
        bootnodes,
        None,
        Some("pcx1"),
        Some(as_properties(NetworkType::Mainnet)),
        Default::default(),
    ))
}

fn chainx_session_keys(
    babe: BabeId,
    grandpa: GrandpaId,
    im_online: ImOnlineId,
    authority_discovery: AuthorityDiscoveryId,
) -> chainx::SessionKeys {
    chainx::SessionKeys {
        grandpa,
        babe,
        im_online,
        authority_discovery,
    }
}

fn mainnet_genesis(
    wasm_binary: &[u8],
    initial_authorities: Vec<AuthorityKeysTuple>,
    assets: Vec<AssetParams>,
    bitcoin: BtcGenesisParams,
    trustees: Vec<(Chain, TrusteeInfoConfig, Vec<BtcTrusteeParams>)>,
) -> chainx::GenesisConfig {
    use chainx_runtime::constants::time::DAYS;

    let (assets, assets_restrictions) = init_assets(assets);
    let tech_comm_members: Vec<AccountId> = vec![
        // 5TdpqWRZxpUxoAvG86TMDrGkbRoRrxqS1noNwmY4xWkvoXsV
        hex!["0221ce7c4a0b771faaf0bbae23c3a1965348cb5257611313a73c3d4a53599509"].into(),
    ];

    let btc_genesis_trustees = trustees
        .iter()
        .find_map(|(chain, _, trustee_params)| {
            if *chain == Chain::Bitcoin {
                Some(
                    trustee_params
                        .iter()
                        .map(|i| (i.0).clone())
                        .collect::<Vec<_>>(),
                )
            } else {
                None
            }
        })
        .expect("bitcoin trustees generation can not fail; qed");

    chainx::GenesisConfig {
        frame_system: Some(chainx::SystemConfig {
            code: wasm_binary.to_vec(),
            changes_trie_config: Default::default(),
        }),
        pallet_babe: Some(Default::default()),
        pallet_grandpa: Some(chainx::GrandpaConfig {
            authorities: vec![],
        }),
        pallet_collective_Instance1: Some(chainx::CouncilConfig::default()),
        pallet_collective_Instance2: Some(chainx::TechnicalCommitteeConfig {
            members: tech_comm_members,
            phantom: Default::default(),
        }),
        pallet_membership_Instance1: Some(Default::default()),
        pallet_democracy: Some(chainx::DemocracyConfig::default()),
        pallet_treasury: Some(Default::default()),
        pallet_elections_phragmen: Some(chainx::ElectionsConfig::default()),
        pallet_im_online: Some(chainx::ImOnlineConfig { keys: vec![] }),
        pallet_authority_discovery: Some(chainx::AuthorityDiscoveryConfig { keys: vec![] }),
        pallet_session: Some(chainx::SessionConfig {
            keys: initial_authorities
                .iter()
                .map(|x| {
                    (
                        (x.0).0.clone(),
                        (x.0).0.clone(),
                        chainx_session_keys(x.1.clone(), x.2.clone(), x.3.clone(), x.4.clone()),
                    )
                })
                .collect::<Vec<_>>(),
        }),
        pallet_balances: Some(chainx::BalancesConfig::default()),
        pallet_indices: Some(chainx::IndicesConfig { indices: vec![] }),
        pallet_sudo: Some(chainx::SudoConfig { key: hex!["b0ca18cce5c51f51655acf683453aa1ff319e3c3edd00b43b36a686a3ae34341"].into() }),
        xpallet_system: Some(chainx::XSystemConfig {
            network_props: NetworkType::Mainnet,
        }),
        xpallet_assets_registrar: Some(chainx::XAssetsRegistrarConfig { assets }),
        xpallet_assets: Some(chainx::XAssetsConfig {
            assets_restrictions,
            endowed: Default::default(),
        }),
        xpallet_gateway_common: Some(chainx::XGatewayCommonConfig { trustees }),
        xpallet_gateway_bitcoin: Some(chainx::XGatewayBitcoinConfig {
            genesis_trustees: btc_genesis_trustees,
            network_id: bitcoin.network,
            confirmation_number: bitcoin.confirmation_number,
            genesis_hash: bitcoin.hash(),
            genesis_info: (bitcoin.header(), bitcoin.height),
            params_info: BtcParams::new(
                486604799,            // max_bits
                2 * 60 * 60,          // block_max_future
                2 * 7 * 24 * 60 * 60, // target_timespan_seconds
                10 * 60,              // target_spacing_seconds
                4,                    // retargeting_factor
            ), // retargeting_factor
            btc_withdrawal_fee: 500000,
            max_withdrawal_count: 100,
            verifier: BtcTxVerifier::Recover,
        }),
        xpallet_mining_staking: Some(chainx::XStakingConfig {
            validator_count: 40,
            sessions_per_era: 12,
            glob_dist_ratio: (12, 88), // (Treasury, X-type Asset and Staking) = (12, 88)
            mining_ratio: (10, 90),    // (Asset Mining, Staking) = (10, 90)
            minimum_penalty: 100 * DOLLARS,
            candidate_requirement: (100 * DOLLARS, 1_000 * DOLLARS), // Minimum value (self_bonded, total_bonded) to be a validator candidate
            ..Default::default()
        }),
        xpallet_mining_asset: Some(chainx::XMiningAssetConfig {
            claim_restrictions: vec![(X_BTC, (10, DAYS * 7))],
            mining_power_map: vec![(X_BTC, 400)],
        }),
        xpallet_dex_spot: Some(chainx::XSpotConfig {
            trading_pairs: vec![(PCX, X_BTC, 9, 2, 100000, true)],
        }),
        xpallet_genesis_builder: Some(chainx::XGenesisBuilderConfig {
            params: crate::genesis::genesis_builder_params(),
            initial_authorities: initial_authorities
                .iter()
                .map(|i| (i.0).1.clone())
                .collect(),
        }),
    }
}
