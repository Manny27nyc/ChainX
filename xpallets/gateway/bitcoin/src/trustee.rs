// Copyright 2019-2020 ChainX Project Authors. Licensed under GPL-3.0.
extern crate alloc;

use alloc::string::ToString;
use frame_support::{
    dispatch::{DispatchError, DispatchResult},
    ensure,
};
use sp_runtime::SaturatedConversion;
use sp_std::{
    cmp::max,
    convert::{TryFrom, TryInto},
    prelude::*,
};

use light_bitcoin::{
    chain::{Transaction, TransactionOutput},
    crypto::dhash160,
    keys::{Address, AddressTypes, Public, Type},
    mast::{compute_min_threshold, Mast},
    primitives::Bytes,
    script::{Builder, Opcode, Script},
};

use xp_gateway_bitcoin::extract_output_addr;
use xpallet_assets::Chain;
use xpallet_gateway_common::{
    traits::{TrusteeForChain, TrusteeSession},
    trustees::bitcoin::{BtcTrusteeAddrInfo, BtcTrusteeType},
    types::{TrusteeInfoConfig, TrusteeIntentionProps, TrusteeSessionInfo},
    utils::two_thirds_unsafe,
};

use crate::tx::validator::parse_check_taproot_tx;
use crate::{
    log,
    tx::{ensure_identical, validator::parse_and_check_signed_tx},
    types::{BtcWithdrawalProposal, VoteResult},
    Config, Error, Event, Pallet, WithdrawalProposal,
};

pub fn current_trustee_session<T: Config>(
) -> Result<TrusteeSessionInfo<T::AccountId, BtcTrusteeAddrInfo>, DispatchError> {
    T::TrusteeSessionProvider::current_trustee_session()
}

#[inline]
fn current_trustee_addr_pair<T: Config>(
) -> Result<(BtcTrusteeAddrInfo, BtcTrusteeAddrInfo), DispatchError> {
    T::TrusteeSessionProvider::current_trustee_session()
        .map(|session_info| (session_info.hot_address, session_info.cold_address))
}

pub fn get_hot_trustee_address<T: Config>() -> Result<Address, DispatchError> {
    current_trustee_addr_pair::<T>()
        .and_then(|(addr_info, _)| Pallet::<T>::verify_btc_address(&addr_info.addr))
}

pub fn get_hot_trustee_redeem_script<T: Config>() -> Result<Script, DispatchError> {
    current_trustee_addr_pair::<T>().map(|(addr_info, _)| addr_info.redeem_script.into())
}

#[inline]
pub fn get_current_trustee_address_pair<T: Config>() -> Result<(Address, Address), DispatchError> {
    current_trustee_addr_pair::<T>().map(|(hot_info, cold_info)| {
        (
            Pallet::<T>::verify_btc_address(&hot_info.addr)
                .expect("should not parse error from storage data; qed"),
            Pallet::<T>::verify_btc_address(&cold_info.addr)
                .expect("should not parse error from storage data; qed"),
        )
    })
}

#[inline]
pub fn get_last_trustee_address_pair<T: Config>() -> Result<(Address, Address), DispatchError> {
    T::TrusteeSessionProvider::last_trustee_session().map(|session_info| {
        (
            Pallet::<T>::verify_btc_address(&session_info.hot_address.addr)
                .expect("should not parse error from storage data; qed"),
            Pallet::<T>::verify_btc_address(&session_info.cold_address.addr)
                .expect("should not parse error from storage data; qed"),
        )
    })
}

fn check_keys<T: Config>(keys: &[Public]) -> DispatchResult {
    let has_duplicate = (1..keys.len()).any(|i| keys[i..].contains(&keys[i - 1]));
    if has_duplicate {
        log!(
            error,
            "[generate_new_trustees] Keys contains duplicate pubkey"
        );
        return Err(Error::<T>::DuplicatedKeys.into());
    }
    let has_normal_pubkey = keys
        .iter()
        .any(|public: &Public| matches!(public, Public::Normal(_)));
    if has_normal_pubkey {
        return Err("Unexpect! All keys(bitcoin Public) should be compressed".into());
    }
    Ok(())
}

//const EC_P = Buffer.from('fffffffffffffffffffffffffffffffffffffffffffffffffffffffefffffc2f', 'hex')
const EC_P: [u8; 32] = [
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 254, 255, 255, 252, 47,
];

const ZERO_P: [u8; 32] = [0; 32];

const MAX_TAPROOT_NODES: u32 = 250;

impl<T: Config> TrusteeForChain<T::AccountId, BtcTrusteeType, BtcTrusteeAddrInfo> for Pallet<T> {
    fn check_trustee_entity(raw_addr: &[u8]) -> Result<BtcTrusteeType, DispatchError> {
        let trustee_type = BtcTrusteeType::try_from(raw_addr.to_vec())
            .map_err(|_| Error::<T>::InvalidPublicKey)?;
        let public = trustee_type.0;
        if let Public::Normal(_) = public {
            log!(error, "Disallow Normal Public for bitcoin now");
            return Err(Error::<T>::InvalidPublicKey.into());
        }

        if 2 != raw_addr[0] && 3 != raw_addr[0] {
            log!(error, "Not Compressed Public(prefix not 2|3)");
            return Err(Error::<T>::InvalidPublicKey.into());
        }

        if ZERO_P == raw_addr[1..33] {
            log!(error, "Not Compressed Public(Zero32)");
            return Err(Error::<T>::InvalidPublicKey.into());
        }

        if raw_addr[1..33].to_vec() >= EC_P.to_vec() {
            log!(error, "Not Compressed Public(EC_P)");
            return Err(Error::<T>::InvalidPublicKey.into());
        }

        Ok(BtcTrusteeType(public))
    }

    fn generate_trustee_session_info(
        props: Vec<(T::AccountId, TrusteeIntentionProps<BtcTrusteeType>)>,
        config: TrusteeInfoConfig,
    ) -> Result<TrusteeSessionInfo<T::AccountId, BtcTrusteeAddrInfo>, DispatchError> {
        // judge all props has different pubkey
        // check
        let (trustees, props_info): (
            Vec<T::AccountId>,
            Vec<TrusteeIntentionProps<BtcTrusteeType>>,
        ) = props.into_iter().unzip();

        let (hot_keys, cold_keys): (Vec<Public>, Vec<Public>) = props_info
            .into_iter()
            .map(|props| (props.hot_entity.0, props.cold_entity.0))
            .unzip();

        check_keys::<T>(&hot_keys)?;
        check_keys::<T>(&cold_keys)?;

        // [min, max] e.g. bitcoin min is 4, max is 15
        if (trustees.len() as u32) < config.min_trustee_count
            || (trustees.len() as u32) > config.max_trustee_count
        {
            log!(
                error,
                "[generate_trustee_session_info] Trustees {:?} is less/more than {{min:{}, max:{}}} people, \
                can't generate trustee addr",
                trustees, config.min_trustee_count, config.max_trustee_count
            );
            return Err(Error::<T>::InvalidTrusteeCount.into());
        }

        #[cfg(feature = "std")]
        let pretty_print_keys = |keys: &[Public]| {
            keys.iter()
                .map(|k| k.to_string().replace("\n", ""))
                .collect::<Vec<_>>()
                .join(", ")
        };
        #[cfg(feature = "std")]
        log!(
            info,
            "[generate_trustee_session_info] hot_keys:[{}], cold_keys:[{}]",
            pretty_print_keys(&hot_keys),
            pretty_print_keys(&cold_keys)
        );

        #[cfg(not(feature = "std"))]
        log!(
            info,
            "[generate_trustee_session_info] hot_keys:{:?}, cold_keys:{:?}",
            hot_keys,
            cold_keys
        );

        let sig_num = max(
            two_thirds_unsafe(trustees.len() as u32),
            compute_min_threshold(trustees.len(), MAX_TAPROOT_NODES as usize) as u32,
        );

        // Set hot address for taproot threshold address
        let pks = hot_keys
            .into_iter()
            .map(|k| k.try_into().map_err(|_| Error::<T>::InvalidPublicKey))
            .collect::<Result<Vec<_>, Error<T>>>()?;

        let threshold_addr: Address = Mast::new(pks, sig_num as usize)
            .map_err(|_| Error::<T>::InvalidAddress)?
            .generate_address(&Pallet::<T>::network_id().to_string())
            .map_err(|_| Error::<T>::InvalidAddress)?
            .parse()
            .map_err(|_| Error::<T>::InvalidAddress)?;

        let hot_trustee_addr_info: BtcTrusteeAddrInfo = BtcTrusteeAddrInfo {
            addr: threshold_addr.to_string().into_bytes(),
            redeem_script: vec![],
        };

        let cold_trustee_addr_info: BtcTrusteeAddrInfo =
            create_multi_address::<T>(&cold_keys, sig_num).ok_or_else(|| {
                log!(
                    error,
                    "[generate_trustee_session_info] Create cold_addr error, cold_keys:{:?}",
                    cold_keys
                );
                Error::<T>::GenerateMultisigFailed
            })?;

        log!(
            info,
            "[generate_trustee_session_info] hot_addr:{:?}, cold_addr:{:?}, trustee_list:{:?}",
            hot_trustee_addr_info,
            cold_trustee_addr_info,
            trustees
        );

        Ok(TrusteeSessionInfo {
            trustee_list: trustees,
            threshold: sig_num as u16,
            hot_address: hot_trustee_addr_info,
            cold_address: cold_trustee_addr_info,
        })
    }
}

impl<T: Config> Pallet<T> {
    pub fn ensure_trustee(who: &T::AccountId) -> DispatchResult {
        let trustee_session_info = current_trustee_session::<T>()?;
        if trustee_session_info.trustee_list.iter().any(|n| n == who) {
            Ok(())
        } else {
            log!(
                error,
                "[ensure_trustee] Committer {:?} not in the trustee list:{:?}",
                who,
                trustee_session_info.trustee_list
            );
            Err(Error::<T>::NotTrustee.into())
        }
    }

    pub fn apply_create_withdraw(
        who: T::AccountId,
        tx: Transaction,
        withdrawal_id_list: Vec<u32>,
    ) -> DispatchResult {
        let withdraw_amount = Self::max_withdrawal_count();
        if withdrawal_id_list.len() > withdraw_amount as usize {
            log!(
                error,
                "[apply_create_withdraw] Current list (len:{}) exceeding the max withdrawal amount {}",
                withdrawal_id_list.len(), withdraw_amount
            );
            return Err(Error::<T>::WroungWithdrawalCount.into());
        }
        // remove duplicate
        let mut withdrawal_id_list = withdrawal_id_list;
        withdrawal_id_list.sort_unstable();
        withdrawal_id_list.dedup();

        check_withdraw_tx::<T>(&tx, &withdrawal_id_list)?;
        log!(
            info,
            "[apply_create_withdraw] Create new withdraw, id_list:{:?}",
            withdrawal_id_list
        );

        // check sig
        let sigs_count = parse_and_check_signed_tx::<T>(&tx)?;
        let apply_sig = if sigs_count == 0 {
            false
        } else if sigs_count == 1 {
            true
        } else {
            log!(
                error,
                "[apply_create_withdraw] The sigs for tx could not more than 1, current sigs:{}",
                sigs_count
            );
            return Err(Error::<T>::InvalidSignCount.into());
        };

        xpallet_gateway_records::Pallet::<T>::process_withdrawals(
            &withdrawal_id_list,
            Chain::Bitcoin,
        )?;

        let mut proposal = BtcWithdrawalProposal::new(
            VoteResult::Unfinish,
            withdrawal_id_list.clone(),
            tx,
            Vec::new(),
        );

        log!(
            info,
            "[apply_create_withdraw] Pass the legality check of withdrawal"
        );

        Self::deposit_event(Event::<T>::WithdrawalProposalCreated(
            who.clone(),
            withdrawal_id_list,
        ));

        if apply_sig {
            log!(
                info,
                "[apply_create_withdraw] Apply sign after creating proposal"
            );
            // due to `SignWithdrawalProposal` event should after `WithdrawalProposalCreated`, thus this function should after proposal
            // but this function would have an error return, this error return should not meet.
            if insert_trustee_vote_state::<T>(true, &who, &mut proposal.trustee_list).is_err() {
                // should not be error in this function, if hit this branch, panic to clear all modification
                // TODO change to revoke in future
                panic!("insert_trustee_vote_state should not be error")
            }
        }

        WithdrawalProposal::<T>::put(proposal);

        Ok(())
    }

    pub fn apply_create_taproot_withdraw(
        who: T::AccountId,
        tx: Transaction,
        withdrawal_id_list: Vec<u32>,
        spent_outputs: Vec<TransactionOutput>,
    ) -> DispatchResult {
        let withdraw_amount = Self::max_withdrawal_count();
        if withdrawal_id_list.len() > withdraw_amount as usize {
            log!(
                error,
                "[apply_create_withdraw] Current list (len:{}) exceeding the max withdrawal amount {}",
                withdrawal_id_list.len(), withdraw_amount
            );
            return Err(Error::<T>::WroungWithdrawalCount.into());
        }
        // remove duplicate
        let mut withdrawal_id_list = withdrawal_id_list;
        withdrawal_id_list.sort_unstable();
        withdrawal_id_list.dedup();

        check_withdraw_tx::<T>(&tx, &withdrawal_id_list)?;
        log!(
            info,
            "[apply_create_withdraw] Create new withdraw, id_list:{:?}",
            withdrawal_id_list
        );

        // check sig
        if parse_check_taproot_tx::<T>(&tx, &spent_outputs).is_err() {
            return Err(Error::<T>::VerifySignFailed.into());
        };

        xpallet_gateway_records::Pallet::<T>::process_withdrawals(
            &withdrawal_id_list,
            Chain::Bitcoin,
        )?;

        let proposal = BtcWithdrawalProposal::new(
            VoteResult::Finish,
            withdrawal_id_list.clone(),
            tx,
            Vec::new(),
        );

        log!(
            info,
            "[apply_create_withdraw] Pass the legality check of withdrawal"
        );

        Self::deposit_event(Event::<T>::WithdrawalProposalCreated(
            who,
            withdrawal_id_list,
        ));

        WithdrawalProposal::<T>::put(proposal);

        Ok(())
    }

    pub fn apply_sig_withdraw(who: T::AccountId, tx: Option<Transaction>) -> DispatchResult {
        let mut proposal: BtcWithdrawalProposal<T::AccountId> =
            Self::withdrawal_proposal().ok_or(Error::<T>::NoProposal)?;

        if proposal.sig_state == VoteResult::Finish {
            log!(error, "[apply_sig_withdraw] Proposal is on FINISH state, can't sign for this proposal:{:?}", proposal);
            return Err(Error::<T>::RejectSig.into());
        }

        let (sig_num, total) = get_sig_num::<T>();
        match tx {
            Some(tx) => {
                // check this tx is same to proposal, just check input and output, not include sigs
                ensure_identical::<T>(&tx, &proposal.tx)?;

                // sign
                // check first and get signatures from commit transaction
                let sigs_count = parse_and_check_signed_tx::<T>(&tx)?;
                if sigs_count == 0 {
                    log!(error, "[apply_sig_withdraw] Tx sig should not be zero, zero is the source tx without any sig, tx{:?}", tx);
                    return Err(Error::<T>::InvalidSignCount.into());
                }

                let confirmed_count = proposal
                    .trustee_list
                    .iter()
                    .filter(|(_, vote)| *vote)
                    .count() as u32;

                if sigs_count != confirmed_count + 1 {
                    log!(
                        error,
                        "[apply_sig_withdraw] Need to sign on the latest signature results, sigs count:{}, confirmed count:{}",
                        sigs_count, confirmed_count
                    );
                    return Err(Error::<T>::InvalidSignCount.into());
                }

                insert_trustee_vote_state::<T>(true, &who, &mut proposal.trustee_list)?;
                // check required count
                // required count should be equal or more than (2/3)*total
                // e.g. total=6 => required=2*6/3=4, thus equal to 4 should mark as finish
                if sigs_count == sig_num {
                    // mark as finish, can't do anything for this proposal
                    log!(
                        info,
                        "[apply_sig_withdraw] Signature completed:{}",
                        sigs_count
                    );
                    proposal.sig_state = VoteResult::Finish;

                    Self::deposit_event(Event::<T>::WithdrawalProposalCompleted(tx.hash()))
                } else {
                    proposal.sig_state = VoteResult::Unfinish;
                }
                // update tx
                proposal.tx = tx;
            }
            None => {
                // reject
                insert_trustee_vote_state::<T>(false, &who, &mut proposal.trustee_list)?;

                let reject_count = proposal
                    .trustee_list
                    .iter()
                    .filter(|(_, vote)| !(*vote))
                    .count() as u32;

                // reject count just need  < (total-required) / total
                // e.g. total=6 => required=2*6/3=4, thus, reject should more than (6-4) = 2
                // > 2 equal to total - required + 1 = 6-4+1 = 3
                let need_reject = total - sig_num + 1;
                if reject_count == need_reject {
                    log!(
                        info,
                        "[apply_sig_withdraw] {}/{} opposition, clear withdrawal proposal",
                        reject_count,
                        total
                    );

                    // release withdrawal for applications
                    for id in proposal.withdrawal_id_list.iter() {
                        let _ = xpallet_gateway_records::Pallet::<T>::recover_withdrawal(
                            *id,
                            Chain::Bitcoin,
                        );
                    }

                    WithdrawalProposal::<T>::kill();

                    Self::deposit_event(Event::<T>::WithdrawalProposalDropped(
                        reject_count as u32,
                        sig_num as u32,
                        proposal.withdrawal_id_list,
                    ));
                    return Ok(());
                }
            }
        }

        log!(
            info,
            "[apply_sig_withdraw] Current sig state:{:?}, trustee vote:{:?}",
            proposal.sig_state,
            proposal.trustee_list
        );

        WithdrawalProposal::<T>::put(proposal);
        Ok(())
    }

    pub fn force_replace_withdraw_tx(tx: Transaction) -> DispatchResult {
        let mut proposal: BtcWithdrawalProposal<T::AccountId> =
            Self::withdrawal_proposal().ok_or(Error::<T>::NoProposal)?;

        ensure!(
            proposal.sig_state == VoteResult::Finish,
            "Only allow force change finished vote"
        );

        // make sure withdrawal list is same as current proposal
        let current_withdrawal_list = &proposal.withdrawal_id_list;
        check_withdraw_tx_impl::<T>(&tx, current_withdrawal_list)?;

        // sign
        // check first and get signatures from commit transaction
        let sigs_count = parse_and_check_signed_tx::<T>(&tx)?;
        ensure!(
            proposal.trustee_list.len() as u32 == sigs_count,
            Error::<T>::InvalidSignCount
        );

        // replace old transaction
        proposal.tx = tx;

        WithdrawalProposal::<T>::put(proposal);
        Ok(())
    }
}

/// Get the required number of signatures
/// sig_num: Number of signatures required
/// trustee_num: Total number of multiple signatures
/// NOTE: Signature ratio greater than 2/3
pub fn get_sig_num<T: Config>() -> (u32, u32) {
    let trustee_list = T::TrusteeSessionProvider::current_trustee_session()
        .map(|session_info| session_info.trustee_list)
        .expect("the trustee_list must exist; qed");
    let trustee_num = trustee_list.len() as u32;
    (two_thirds_unsafe(trustee_num), trustee_num)
}

pub(crate) fn create_multi_address<T: Config>(
    pubkeys: &[Public],
    sig_num: u32,
) -> Option<BtcTrusteeAddrInfo> {
    let sum = pubkeys.len() as u32;
    if sig_num > sum {
        panic!("required sig num should less than trustee_num; qed")
    }
    if sum > 15 {
        log!(
            error,
            "Bitcoin's multisig can't more than 15, current:{}",
            sum
        );
        return None;
    }

    let opcode = match Opcode::from_u8(Opcode::OP_1 as u8 + sig_num as u8 - 1) {
        Some(o) => o,
        None => return None,
    };
    let mut build = Builder::default().push_opcode(opcode);
    for pubkey in pubkeys.iter() {
        build = build.push_bytes(pubkey);
    }

    let opcode = match Opcode::from_u8(Opcode::OP_1 as u8 + sum as u8 - 1) {
        Some(o) => o,
        None => return None,
    };
    let redeem_script = build
        .push_opcode(opcode)
        .push_opcode(Opcode::OP_CHECKMULTISIG)
        .into_script();

    let addr = Address {
        kind: Type::P2SH,
        network: Pallet::<T>::network_id(),
        hash: AddressTypes::Legacy(dhash160(&redeem_script)),
    };
    let script_bytes: Bytes = redeem_script.into();
    Some(BtcTrusteeAddrInfo {
        addr: addr.to_string().into_bytes(),
        redeem_script: script_bytes.into(),
    })
}

/// Update the signature status of trustee
/// state: false -> Veto signature, true -> Consent signature
/// only allow inseRelayedTx once
fn insert_trustee_vote_state<T: Config>(
    state: bool,
    who: &T::AccountId,
    trustee_list: &mut Vec<(T::AccountId, bool)>,
) -> DispatchResult {
    match trustee_list.iter_mut().find(|info| info.0 == *who) {
        Some(_) => {
            // if account is exist, override state
            log!(error, "[insert_trustee_vote_state] {:?} has already vote for this withdrawal proposal, old vote:{}", who, state);
            return Err(Error::<T>::DuplicateVote.into());
        }
        None => {
            trustee_list.push((who.clone(), state));
            log!(
                debug,
                "[insert_trustee_vote_state] Insert new vote, who:{:?}, state:{}",
                who,
                state
            );
        }
    }
    Pallet::<T>::deposit_event(Event::<T>::WithdrawalProposalVoted(who.clone(), state));
    Ok(())
}

/// Check that the cash withdrawal transaction is correct
fn check_withdraw_tx<T: Config>(tx: &Transaction, withdrawal_id_list: &[u32]) -> DispatchResult {
    match Pallet::<T>::withdrawal_proposal() {
        Some(_) => Err(Error::<T>::NotFinishProposal.into()),
        None => check_withdraw_tx_impl::<T>(tx, withdrawal_id_list),
    }
}

fn check_withdraw_tx_impl<T: Config>(
    tx: &Transaction,
    withdrawal_id_list: &[u32],
) -> DispatchResult {
    // withdrawal addr list for account withdrawal application
    let mut appl_withdrawal_list: Vec<(Address, u64)> = Vec::new();
    for withdraw_index in withdrawal_id_list.iter() {
        let record = xpallet_gateway_records::Pallet::<T>::pending_withdrawals(withdraw_index)
            .ok_or(Error::<T>::NoWithdrawalRecord)?;
        // record.addr() is base58
        // verify btc address would conveRelayedTx a base58 addr to Address
        let addr: Address = Pallet::<T>::verify_btc_address(record.addr())?;

        appl_withdrawal_list.push((addr, record.balance().saturated_into::<u64>()));
    }
    // not allow deposit directly to cold address, only hot address allow
    let hot_trustee_address: Address = get_hot_trustee_address::<T>()?;
    // withdrawal addr list for tx outputs
    let btc_withdrawal_fee = Pallet::<T>::btc_withdrawal_fee();
    let btc_network = Pallet::<T>::network_id();
    let mut tx_withdraw_list = Vec::new();
    for output in &tx.outputs {
        let addr = extract_output_addr(output, btc_network).ok_or("not found addr in this out")?;
        if addr.hash != hot_trustee_address.hash {
            // expect change to trustee_addr output
            tx_withdraw_list.push((addr, output.value + btc_withdrawal_fee));
        }
    }

    tx_withdraw_list.sort();
    appl_withdrawal_list.sort();

    // appl_withdrawal_list must match to tx_withdraw_list
    if appl_withdrawal_list.len() != tx_withdraw_list.len() {
        log!(
            error,
            "Withdrawal tx's outputs (len:{}) != withdrawal application list (len:{}), \
            withdrawal tx's outputs:{:?}, withdrawal application list:{:?}",
            tx_withdraw_list.len(),
            appl_withdrawal_list.len(),
            tx_withdraw_list,
            withdrawal_id_list
                .iter()
                .zip(appl_withdrawal_list)
                .collect::<Vec<_>>()
        );
        return Err(Error::<T>::InvalidProposal.into());
    }

    let count = appl_withdrawal_list
        .iter()
        .zip(tx_withdraw_list)
        .filter(|(a, b)| {
            if a.0 == b.0 && a.1 == b.1 {
                true
            } else {
                log!(
                    error,
                    "Withdrawal tx's output not match to withdrawal application. \
                    withdrawal application:{:?}, tx withdrawal output:{:?}",
                    a,
                    b
                );
                false
            }
        })
        .count();

    if count != appl_withdrawal_list.len() {
        return Err(Error::<T>::InvalidProposal.into());
    }

    Ok(())
}
