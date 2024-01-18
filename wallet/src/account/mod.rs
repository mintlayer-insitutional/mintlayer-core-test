// Copyright (c) 2023 RBB S.r.l
// opensource@mintlayer.org
// SPDX-License-Identifier: MIT
// Licensed under the MIT License;
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://github.com/mintlayer/mintlayer-core/blob/master/LICENSE
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

mod output_cache;
pub mod transaction_list;
mod utxo_selector;

use common::address::pubkeyhash::PublicKeyHash;
use common::chain::block::timestamp::BlockTimestamp;
use common::chain::{AccountCommand, AccountOutPoint, AccountSpending, TransactionCreationError};
use common::primitives::id::WithId;
use common::primitives::{Idable, H256};
use common::size_estimation::{
    input_signature_size, input_signature_size_from_destination, tx_size_with_outputs,
};
use common::Uint256;
use crypto::key::hdkd::child_number::ChildNumber;
use mempool::FeeRate;
use utils::ensure;
pub use utxo_selector::UtxoSelectorError;
use wallet_types::account_id::AccountPrefixedId;
use wallet_types::with_locked::WithLocked;

use crate::account::utxo_selector::{select_coins, OutputGroup};
use crate::key_chain::{AccountKeyChain, KeyChainError};
use crate::send_request::{
    get_reward_output_destination, make_address_output, make_address_output_from_delegation,
    make_address_output_token, make_decommission_stake_pool_output, make_mint_token_outputs,
    make_stake_output, make_unmint_token_outputs, IssueNftArguments, StakePoolDataArguments,
};
use crate::wallet_events::{WalletEvents, WalletEventsNoOp};
use crate::{get_tx_output_destination, SendRequest, WalletError, WalletResult};
use common::address::Address;
use common::chain::output_value::OutputValue;
use common::chain::signature::inputsig::standard_signature::StandardInputSignature;
use common::chain::signature::inputsig::InputWitness;
use common::chain::signature::sighash::sighashtype::SigHashType;
use common::chain::tokens::{
    make_token_id, IsTokenUnfreezable, NftIssuance, NftIssuanceV0, RPCFungibleTokenInfo, TokenId,
};
use common::chain::{
    AccountNonce, Block, ChainConfig, DelegationId, Destination, GenBlock, PoolId,
    SignedTransaction, Transaction, TxInput, TxOutput, UtxoOutPoint,
};
use common::primitives::{Amount, BlockHeight, Id};
use consensus::PoSGenerateBlockInputData;
use crypto::key::hdkd::u31::U31;
use crypto::key::PublicKey;
use crypto::vrf::VRFPublicKey;
use itertools::Itertools;
use serialization::{Decode, Encode};
use std::cmp::Reverse;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::{Add, Sub};
use std::sync::Arc;
use wallet_storage::{
    StoreTxRw, WalletStorageReadLocked, WalletStorageReadUnlocked, WalletStorageWriteLocked,
    WalletStorageWriteUnlocked,
};
use wallet_types::utxo_types::{get_utxo_type, UtxoState, UtxoStates, UtxoType, UtxoTypes};
use wallet_types::wallet_tx::{BlockData, TxData, TxState};
use wallet_types::{
    AccountId, AccountInfo, AccountWalletCreatedTxId, AccountWalletTxId, BlockInfo, KeyPurpose,
    KeychainUsageState, WalletTx,
};

pub use self::output_cache::{
    DelegationData, FungibleTokenInfo, PoolData, UnconfirmedTokenInfo, UtxoWithTxOutput,
};
use self::output_cache::{OutputCache, TokenIssuanceData};
use self::transaction_list::{get_transaction_list, TransactionList};
use self::utxo_selector::{CoinSelectionAlgo, PayFee};

pub struct CurrentFeeRate {
    pub current_fee_rate: FeeRate,
    pub consolidate_fee_rate: FeeRate,
}

#[derive(Debug, Eq, PartialEq, Clone, Encode, Decode)]
pub struct PartiallySignedTransaction {
    tx: Transaction,
    witnesses: Vec<Option<InputWitness>>,
}

impl PartiallySignedTransaction {
    pub fn new(tx: Transaction, witnesses: Vec<Option<InputWitness>>) -> Self {
        Self { tx, witnesses }
    }

    pub fn count_inputs(&self) -> usize {
        self.tx.inputs().len()
    }

    pub fn count_completed_signatures(&self) -> usize {
        self.witnesses.iter().filter(|w| w.is_some()).count()
    }

    pub fn is_fully_signed(&self) -> bool {
        self.witnesses.iter().all(|w| w.is_some())
    }

    pub fn into_signed_tx(self) -> WalletResult<SignedTransaction> {
        if self.is_fully_signed() {
            let witnesses = self.witnesses.into_iter().map(|w| w.expect("cannot fail")).collect();
            Ok(SignedTransaction::new(self.tx, witnesses)?)
        } else {
            Err(WalletError::FailedToConvertPartiallySignedTx(self))
        }
    }

    pub fn take(self) -> (Transaction, Vec<Option<InputWitness>>) {
        (self.tx, self.witnesses)
    }
}

pub struct Account {
    chain_config: Arc<ChainConfig>,
    key_chain: AccountKeyChain,
    output_cache: OutputCache,
    account_info: AccountInfo,
}

impl Account {
    pub fn load_from_database(
        chain_config: Arc<ChainConfig>,
        db_tx: &impl WalletStorageReadLocked,
        id: &AccountId,
    ) -> WalletResult<Account> {
        let mut account_infos = db_tx.get_accounts_info()?;
        let account_info =
            account_infos.remove(id).ok_or(KeyChainError::NoAccountFound(id.clone()))?;

        let key_chain =
            AccountKeyChain::load_from_database(chain_config.clone(), db_tx, id, &account_info)?;

        let txs = db_tx.get_transactions(&key_chain.get_account_id())?;
        let output_cache = OutputCache::new(txs)?;

        Ok(Account {
            chain_config,
            key_chain,
            output_cache,
            account_info,
        })
    }

    /// Create a new account by providing a key chain
    pub fn new(
        chain_config: Arc<ChainConfig>,
        db_tx: &mut impl WalletStorageWriteLocked,
        key_chain: AccountKeyChain,
        name: Option<String>,
    ) -> WalletResult<Account> {
        let account_id = key_chain.get_account_id();

        let account_info = AccountInfo::new(
            &chain_config,
            key_chain.account_index(),
            key_chain.account_public_key().clone(),
            key_chain.lookahead_size(),
            name,
        );

        db_tx.set_account(&account_id, &account_info)?;
        db_tx.set_account_unconfirmed_tx_counter(&account_id, 0)?;

        let output_cache = OutputCache::empty();

        let mut account = Account {
            chain_config,
            key_chain,
            output_cache,
            account_info,
        };

        account.scan_genesis(db_tx, &WalletEventsNoOp)?;

        Ok(account)
    }

    pub fn find_used_tokens(
        &self,
        input_utxos: &[UtxoOutPoint],
        median_time: BlockTimestamp,
    ) -> WalletResult<BTreeSet<TokenId>> {
        let current_block_info = BlockInfo {
            height: self.account_info.best_block_height(),
            timestamp: median_time,
        };
        self.output_cache.find_used_tokens(current_block_info, input_utxos)
    }

    fn select_inputs_for_send_request(
        &mut self,
        request: SendRequest,
        input_utxos: Vec<UtxoOutPoint>,
        db_tx: &mut impl WalletStorageWriteLocked,
        median_time: BlockTimestamp,
        current_fee_rate: FeeRate,
        consolidate_fee_rate: FeeRate,
    ) -> WalletResult<SendRequest> {
        // TODO: allow to pay fees with different currency?
        let pay_fee_with_currency = Currency::Coin;

        let mut output_currency_amounts = group_outputs_with_issuance_fee(
            request.outputs().iter(),
            |&output| output,
            |grouped: &mut Amount, _, new_amount| -> WalletResult<()> {
                *grouped = grouped.add(new_amount).ok_or(WalletError::OutputAmountOverflow)?;
                Ok(())
            },
            Amount::ZERO,
            &self.chain_config,
        )?;

        let network_fee: Amount = current_fee_rate
            .compute_fee(tx_size_with_outputs(request.outputs()))
            .map_err(|_| UtxoSelectorError::AmountArithmeticError)?
            .into();

        let (coin_change_fee, token_change_fee) =
            coin_and_token_output_change_fees(current_fee_rate)?;

        let mut preselected_inputs =
            group_preselected_inputs(&request, current_fee_rate, &self.chain_config)?;

        let (utxos, selection_algo) = if input_utxos.is_empty() {
            (
                self.get_utxos(
                    UtxoType::Transfer | UtxoType::LockThenTransfer | UtxoType::IssueNft,
                    median_time,
                    UtxoState::Confirmed | UtxoState::InMempool | UtxoState::Inactive,
                    WithLocked::Unlocked,
                ),
                CoinSelectionAlgo::Randomize,
            )
        } else {
            let current_block_info = BlockInfo {
                height: self.account_info.best_block_height(),
                timestamp: median_time,
            };
            (
                self.output_cache.find_utxos(current_block_info, input_utxos)?,
                CoinSelectionAlgo::UsePreselected,
            )
        };

        let mut utxos_by_currency = self.utxo_output_groups_by_currency(
            current_fee_rate,
            consolidate_fee_rate,
            &pay_fee_with_currency,
            utxos,
        )?;

        let amount_to_be_paid_in_currency_with_fees =
            output_currency_amounts.remove(&pay_fee_with_currency).unwrap_or(Amount::ZERO);

        let mut total_fees_not_paid = network_fee;

        let mut selected_inputs: BTreeMap<_, _> = output_currency_amounts
            .iter()
            .map(|(currency, output_amount)| -> WalletResult<_> {
                let utxos = utxos_by_currency.remove(currency).unwrap_or(vec![]);
                let (preselected_amount, preselected_fee) =
                    preselected_inputs.remove(currency).unwrap_or((Amount::ZERO, Amount::ZERO));

                let cost_of_change = match currency {
                    Currency::Coin => coin_change_fee,
                    Currency::Token(_) => token_change_fee,
                };
                let selection_result = select_coins(
                    utxos,
                    output_amount.sub(preselected_amount).unwrap_or(Amount::ZERO),
                    PayFee::DoNotPayFeeWithThisCurrency,
                    // TODO: change this to cost_of_change calculated in this currency
                    // when we allow paying fees with different currency
                    Amount::ZERO,
                    selection_algo,
                )?;

                total_fees_not_paid = (total_fees_not_paid + selection_result.get_total_fees())
                    .ok_or(WalletError::OutputAmountOverflow)?;
                total_fees_not_paid = (total_fees_not_paid + preselected_fee)
                    .ok_or(WalletError::OutputAmountOverflow)?;

                let preselected_change =
                    (preselected_amount - *output_amount).unwrap_or(Amount::ZERO);
                let selection_result = selection_result.add_change(preselected_change)?;
                let change_amount = selection_result.get_change();
                if change_amount > Amount::ZERO {
                    total_fees_not_paid = (total_fees_not_paid + cost_of_change)
                        .ok_or(WalletError::OutputAmountOverflow)?;
                }

                Ok((currency.clone(), selection_result))
            })
            .try_collect()?;

        let utxos = utxos_by_currency.remove(&pay_fee_with_currency).unwrap_or(vec![]);
        let (preselected_amount, preselected_fee) = preselected_inputs
            .remove(&pay_fee_with_currency)
            .unwrap_or((Amount::ZERO, Amount::ZERO));

        total_fees_not_paid =
            (total_fees_not_paid + preselected_fee).ok_or(WalletError::OutputAmountOverflow)?;
        total_fees_not_paid = preselected_inputs
            .values()
            .try_fold(total_fees_not_paid, |total, (_amount, fee)| total + *fee)
            .ok_or(WalletError::OutputAmountOverflow)?;

        let mut amount_to_be_paid_in_currency_with_fees = (amount_to_be_paid_in_currency_with_fees
            + total_fees_not_paid)
            .ok_or(WalletError::OutputAmountOverflow)?;

        let cost_of_change = match pay_fee_with_currency {
            Currency::Coin => coin_change_fee,
            Currency::Token(_) => token_change_fee,
        };

        let selection_result = select_coins(
            utxos,
            (amount_to_be_paid_in_currency_with_fees - preselected_amount).unwrap_or(Amount::ZERO),
            PayFee::PayFeeWithThisCurrency,
            cost_of_change,
            selection_algo,
        )?;

        let selection_result = selection_result.add_change(
            (preselected_amount - amount_to_be_paid_in_currency_with_fees).unwrap_or(Amount::ZERO),
        )?;
        let change_amount = selection_result.get_change();
        if change_amount > Amount::ZERO {
            amount_to_be_paid_in_currency_with_fees = (amount_to_be_paid_in_currency_with_fees
                + cost_of_change)
                .ok_or(WalletError::OutputAmountOverflow)?;
        }

        output_currency_amounts.insert(
            pay_fee_with_currency.clone(),
            (amount_to_be_paid_in_currency_with_fees + selection_result.get_total_fees())
                .ok_or(WalletError::OutputAmountOverflow)?,
        );
        selected_inputs.insert(pay_fee_with_currency, selection_result);

        // Check outputs against inputs and create change
        self.check_outputs_and_add_change(output_currency_amounts, selected_inputs, db_tx, request)
    }

    fn check_outputs_and_add_change(
        &mut self,
        output_currency_amounts: BTreeMap<Currency, Amount>,
        selected_inputs: BTreeMap<Currency, utxo_selector::SelectionResult>,
        db_tx: &mut impl WalletStorageWriteLocked,
        mut request: SendRequest,
    ) -> Result<SendRequest, WalletError> {
        for currency in output_currency_amounts.keys() {
            let change_amount =
                selected_inputs.get(currency).map_or(Amount::ZERO, |result| result.get_change());

            if change_amount > Amount::ZERO {
                let (_, change_address) = self.get_new_address(db_tx, KeyPurpose::Change)?;
                let change_output = match currency {
                    Currency::Coin => make_address_output(
                        self.chain_config.as_ref(),
                        change_address,
                        change_amount,
                    )?,
                    Currency::Token(token_id) => make_address_output_token(
                        self.chain_config.as_ref(),
                        change_address,
                        change_amount,
                        *token_id,
                    )?,
                };
                request = request.with_outputs([change_output]);
            }
        }

        let selected_inputs = selected_inputs.into_iter().flat_map(|x| x.1.into_output_pairs());

        request.with_inputs(selected_inputs)
    }

    fn utxo_output_groups_by_currency(
        &self,
        current_fee_rate: FeeRate,
        consolidate_fee_rate: FeeRate,
        pay_fee_with_currency: &Currency,
        utxos: Vec<(UtxoOutPoint, (&TxOutput, Option<TokenId>))>,
    ) -> Result<BTreeMap<Currency, Vec<OutputGroup>>, WalletError> {
        let utxo_to_output_group =
            |(outpoint, txo): (UtxoOutPoint, TxOutput)| -> WalletResult<OutputGroup> {
                let tx_input: TxInput = outpoint.into();
                let input_size = serialization::Encode::encoded_size(&tx_input);

                let inp_sig_size = input_signature_size(&txo)?;

                let fee = current_fee_rate
                    .compute_fee(input_size + inp_sig_size)
                    .map_err(|_| UtxoSelectorError::AmountArithmeticError)?;
                let consolidate_fee = consolidate_fee_rate
                    .compute_fee(input_size + inp_sig_size)
                    .map_err(|_| UtxoSelectorError::AmountArithmeticError)?;

                // TODO-#1120: calculate weight from the size of the input
                let weight = 0;
                let out_group =
                    OutputGroup::new((tx_input, txo), fee.into(), consolidate_fee.into(), weight)?;

                Ok(out_group)
            };

        group_utxos_for_input(
            utxos.into_iter(),
            |(_, (tx_output, _))| tx_output,
            |grouped: &mut Vec<(UtxoOutPoint, TxOutput)>, element, _| -> WalletResult<()> {
                grouped.push((element.0.clone(), element.1 .0.clone()));
                Ok(())
            },
            vec![],
        )?
        .into_iter()
        .map(
            |(currency, utxos)| -> WalletResult<(Currency, Vec<OutputGroup>)> {
                let utxo_groups = utxos
                    .into_iter()
                    // TODO: group outputs by destination
                    .map(utxo_to_output_group)
                    .filter(|group| {
                        group.as_ref().map_or(true, |group| {
                            currency != *pay_fee_with_currency || group.value > group.fee
                        })
                    })
                    .try_collect()?;

                Ok((currency, utxo_groups))
            },
        )
        .try_collect()
    }

    pub fn process_send_request(
        &mut self,
        db_tx: &mut impl WalletStorageWriteUnlocked,
        request: SendRequest,
        inputs: Vec<UtxoOutPoint>,
        median_time: BlockTimestamp,
        fee_rate: CurrentFeeRate,
    ) -> WalletResult<SignedTransaction> {
        let request = self.select_inputs_for_send_request(
            request,
            inputs,
            db_tx,
            median_time,
            fee_rate.current_fee_rate,
            fee_rate.consolidate_fee_rate,
        )?;
        // TODO: Randomize inputs and outputs

        let tx = self.sign_transaction_from_req(request, db_tx)?;
        Ok(tx)
    }

    fn decommission_stake_pool_impl(
        &mut self,
        db_tx: &mut impl WalletStorageWriteUnlocked,
        pool_id: PoolId,
        pool_balance: Amount,
        current_fee_rate: FeeRate,
    ) -> WalletResult<PartiallySignedTransaction> {
        let pool_data = self.output_cache.pool_data(pool_id)?;
        let best_block_height = self.best_block().1;
        let tx_input = TxInput::Utxo(pool_data.utxo_outpoint.clone());

        let network_fee: Amount = {
            let output = make_decommission_stake_pool_output(
                self.chain_config.as_ref(),
                pool_data.decommission_key.clone(),
                pool_balance,
                best_block_height,
            )?;
            let outputs = vec![output];

            current_fee_rate
                .compute_fee(
                    tx_size_with_outputs(outputs.as_slice())
                        + input_signature_size_from_destination(&pool_data.decommission_key)?
                        + serialization::Encode::encoded_size(&tx_input),
                )
                .map_err(|_| UtxoSelectorError::AmountArithmeticError)?
                .into()
        };

        let output = make_decommission_stake_pool_output(
            self.chain_config.as_ref(),
            pool_data.decommission_key.clone(),
            (pool_balance - network_fee)
                .ok_or(WalletError::NotEnoughUtxo(network_fee, pool_balance))?,
            best_block_height,
        )?;

        let tx = Transaction::new(0, vec![tx_input], vec![output])?;

        let input_utxo = self.output_cache.get_txo(&pool_data.utxo_outpoint);
        self.sign_transaction(tx, &[&pool_data.decommission_key], &[input_utxo], db_tx)
    }

    pub fn decommission_stake_pool(
        &mut self,
        db_tx: &mut impl WalletStorageWriteUnlocked,
        pool_id: PoolId,
        pool_balance: Amount,
        current_fee_rate: FeeRate,
    ) -> WalletResult<SignedTransaction> {
        let result =
            self.decommission_stake_pool_impl(db_tx, pool_id, pool_balance, current_fee_rate)?;
        result
            .into_signed_tx()
            .map_err(|_| WalletError::PartiallySignedTransactionInDecommissionCommand)
    }

    pub fn decommission_stake_pool_request(
        &mut self,
        db_tx: &mut impl WalletStorageWriteUnlocked,
        pool_id: PoolId,
        pool_balance: Amount,
        current_fee_rate: FeeRate,
    ) -> WalletResult<PartiallySignedTransaction> {
        let result =
            self.decommission_stake_pool_impl(db_tx, pool_id, pool_balance, current_fee_rate)?;
        if result.is_fully_signed() {
            return Err(WalletError::FullySignedTransactionInDecommissionReq);
        }
        Ok(result)
    }

    pub fn spend_from_delegation(
        &mut self,
        db_tx: &mut impl WalletStorageWriteUnlocked,
        address: Address<Destination>,
        amount: Amount,
        delegation_id: DelegationId,
        delegation_share: Amount,
        current_fee_rate: FeeRate,
    ) -> WalletResult<SignedTransaction> {
        let current_block_height = self.best_block().1;
        let output = make_address_output_from_delegation(
            self.chain_config.as_ref(),
            address,
            amount,
            current_block_height,
        )?;
        let delegation_data = self.find_delegation(&delegation_id)?;
        let nonce = delegation_data
            .last_nonce
            .map_or(Some(AccountNonce::new(0)), |nonce| nonce.increment())
            .ok_or(WalletError::DelegationNonceOverflow(delegation_id))?;

        let outputs = vec![output];
        let network_fee: Amount = current_fee_rate
            .compute_fee(
                tx_size_with_outputs(outputs.as_slice())
                    + input_signature_size_from_destination(&delegation_data.destination)?,
            )
            .map_err(|_| UtxoSelectorError::AmountArithmeticError)?
            .into();

        let amount_with_fee = (amount + network_fee).ok_or(WalletError::OutputAmountOverflow)?;
        let mut tx_input = TxInput::Account(AccountOutPoint::new(
            nonce,
            AccountSpending::DelegationBalance(delegation_id, amount_with_fee),
        ));
        // as the input size depends on the amount we specify the fee will also change a bit so
        // loop until it converges.
        let mut input_size = serialization::Encode::encoded_size(&tx_input);
        loop {
            let new_amount_with_fee = (amount_with_fee
                + current_fee_rate
                    .compute_fee(input_size)
                    .map_err(|_| UtxoSelectorError::AmountArithmeticError)?
                    .into())
            .ok_or(WalletError::OutputAmountOverflow)?;
            ensure!(
                new_amount_with_fee <= delegation_share,
                UtxoSelectorError::NotEnoughFunds(delegation_share, new_amount_with_fee)
            );

            tx_input = TxInput::Account(AccountOutPoint::new(
                nonce,
                AccountSpending::DelegationBalance(delegation_id, new_amount_with_fee),
            ));

            let new_input_size = serialization::Encode::encoded_size(&tx_input);
            if new_input_size == input_size {
                break;
            }
            input_size = new_input_size;
        }
        let tx = Transaction::new(0, vec![tx_input], outputs)?;

        self.sign_transaction(tx, &[&delegation_data.destination], &[None], db_tx)?
            .into_signed_tx()
    }

    fn get_vrf_public_key(
        &mut self,
        db_tx: &mut impl WalletStorageWriteLocked,
    ) -> WalletResult<VRFPublicKey> {
        Ok(self.key_chain.issue_vrf_key(db_tx)?.into_public_key())
    }

    pub fn get_pool_ids(&self) -> Vec<(PoolId, PoolData)> {
        self.output_cache.pool_ids()
    }

    pub fn get_delegations(&self) -> impl Iterator<Item = (&DelegationId, &DelegationData)> {
        self.output_cache
            .delegation_ids()
            .filter(|(_, data)| self.is_mine_or_watched_destination(&data.destination))
    }

    pub fn find_delegation(&self, delegation_id: &DelegationId) -> WalletResult<&DelegationData> {
        self.output_cache
            .delegation_data(delegation_id)
            .filter(|data| self.is_mine_or_watched_destination(&data.destination))
            .ok_or(WalletError::DelegationNotFound(*delegation_id))
    }

    pub fn find_token(&self, token_id: &TokenId) -> WalletResult<&TokenIssuanceData> {
        self.output_cache
            .token_data(token_id)
            .filter(|data| self.is_mine_or_watched_destination(&data.authority))
            .ok_or(WalletError::UnknownTokenId(*token_id))
    }

    pub fn get_token_unconfirmed_info(
        &self,
        token_info: &RPCFungibleTokenInfo,
    ) -> WalletResult<UnconfirmedTokenInfo> {
        self.output_cache
            .get_token_unconfirmed_info(token_info, |destination: &Destination| {
                self.is_mine_or_watched_destination(destination)
            })
    }

    pub fn create_stake_pool_tx(
        &mut self,
        db_tx: &mut impl WalletStorageWriteUnlocked,
        stake_pool_arguments: StakePoolDataArguments,
        median_time: BlockTimestamp,
        fee_rate: CurrentFeeRate,
    ) -> WalletResult<SignedTransaction> {
        // TODO: Use other accounts here
        let staker = Destination::PublicKey(
            self.key_chain.issue_key(db_tx, KeyPurpose::ReceiveFunds)?.into_public_key(),
        );
        let vrf_public_key = self.get_vrf_public_key(db_tx)?;

        // the first UTXO is needed in advance to calculate pool_id, so just make a dummy one
        // and then replace it with when we can calculate the pool_id
        let dummy_pool_id = PoolId::new(Uint256::from_u64(0).into());
        let dummy_stake_output =
            make_stake_output(dummy_pool_id, stake_pool_arguments, staker, vrf_public_key);
        let request = SendRequest::new().with_outputs([dummy_stake_output]);
        let mut request = self.select_inputs_for_send_request(
            request,
            vec![],
            db_tx,
            median_time,
            fee_rate.current_fee_rate,
            fee_rate.consolidate_fee_rate,
        )?;

        let new_pool_id = match request
            .inputs()
            .first()
            .expect("selector must have selected something or returned an error")
        {
            TxInput::Utxo(input0_outpoint) => Some(pos_accounting::make_pool_id(input0_outpoint)),
            TxInput::Account(..) | TxInput::AccountCommand(..) => None,
        }
        .ok_or(WalletError::NoUtxos)?;

        // update the dummy_pool_id with the new pool_id
        let old_pool_id = request
            .get_outputs_mut()
            .iter_mut()
            .find_map(|out| match out {
                TxOutput::CreateStakePool(pool_id, _) if *pool_id == dummy_pool_id => Some(pool_id),
                TxOutput::CreateStakePool(_, _)
                | TxOutput::Burn(_)
                | TxOutput::Transfer(_, _)
                | TxOutput::DelegateStaking(_, _)
                | TxOutput::LockThenTransfer(_, _, _)
                | TxOutput::CreateDelegationId(_, _)
                | TxOutput::ProduceBlockFromStake(_, _)
                | TxOutput::IssueFungibleToken(_)
                | TxOutput::IssueNft(_, _, _)
                | TxOutput::DataDeposit(_) => None,
            })
            .expect("find output with dummy_pool_id");
        *old_pool_id = new_pool_id;

        let tx = self.sign_transaction_from_req(request, db_tx)?;
        Ok(tx)
    }

    pub fn create_issue_nft_tx(
        &mut self,
        db_tx: &mut impl WalletStorageWriteUnlocked,
        nft_issue_arguments: IssueNftArguments,
        median_time: BlockTimestamp,
        fee_rate: CurrentFeeRate,
    ) -> WalletResult<SignedTransaction> {
        // the first UTXO is needed in advance to issue a new nft, so just make a dummy one
        // and then replace it with when we can calculate the pool_id
        let dummy_token_id = TokenId::new(H256::zero());
        let dummy_issuance_output = TxOutput::IssueNft(
            dummy_token_id,
            Box::new(NftIssuance::V0(NftIssuanceV0 {
                metadata: nft_issue_arguments.metadata,
            })),
            nft_issue_arguments.destination,
        );

        let request = SendRequest::new().with_outputs([dummy_issuance_output]);
        let mut request = self.select_inputs_for_send_request(
            request,
            vec![],
            db_tx,
            median_time,
            fee_rate.current_fee_rate,
            fee_rate.consolidate_fee_rate,
        )?;

        let new_token_id = make_token_id(request.inputs()).ok_or(WalletError::NoUtxos)?;

        // update the dummy_token_id with the new_token_id
        let old_token_id = request
            .get_outputs_mut()
            .iter_mut()
            .find_map(|output| match output {
                TxOutput::CreateStakePool(_, _)
                | TxOutput::Burn(_)
                | TxOutput::Transfer(_, _)
                | TxOutput::DelegateStaking(_, _)
                | TxOutput::LockThenTransfer(_, _, _)
                | TxOutput::CreateDelegationId(_, _)
                | TxOutput::ProduceBlockFromStake(_, _)
                | TxOutput::IssueFungibleToken(_)
                | TxOutput::DataDeposit(_) => None,
                TxOutput::IssueNft(token_id, _, _) => {
                    (*token_id == dummy_token_id).then_some(token_id)
                }
            })
            .expect("find output with dummy_token_id");
        *old_token_id = new_token_id;

        let tx = self.sign_transaction_from_req(request, db_tx)?;
        Ok(tx)
    }

    pub fn mint_tokens(
        &mut self,
        db_tx: &mut impl WalletStorageWriteUnlocked,
        token_info: &UnconfirmedTokenInfo,
        address: Address<Destination>,
        amount: Amount,
        median_time: BlockTimestamp,
        fee_rate: CurrentFeeRate,
    ) -> WalletResult<SignedTransaction> {
        let token_id = *token_info.token_id();
        let outputs =
            make_mint_token_outputs(token_id, amount, address, self.chain_config.as_ref())?;

        token_info.check_can_mint(amount)?;

        let nonce = token_info.get_next_nonce()?;
        let tx_input = TxInput::AccountCommand(nonce, AccountCommand::MintTokens(token_id, amount));
        let authority = token_info.authority()?.clone();

        self.change_token_supply_transaction(
            authority,
            tx_input,
            outputs,
            db_tx,
            median_time,
            fee_rate,
        )
    }

    pub fn unmint_tokens(
        &mut self,
        db_tx: &mut impl WalletStorageWriteUnlocked,
        token_info: &UnconfirmedTokenInfo,
        amount: Amount,
        median_time: BlockTimestamp,
        fee_rate: CurrentFeeRate,
    ) -> WalletResult<SignedTransaction> {
        let token_id = *token_info.token_id();
        let outputs = make_unmint_token_outputs(token_id, amount);

        token_info.check_can_unmint(amount)?;

        let nonce = token_info.get_next_nonce()?;
        let tx_input = TxInput::AccountCommand(nonce, AccountCommand::UnmintTokens(token_id));
        let authority = token_info.authority()?.clone();

        self.change_token_supply_transaction(
            authority,
            tx_input,
            outputs,
            db_tx,
            median_time,
            fee_rate,
        )
    }

    pub fn lock_token_supply(
        &mut self,
        db_tx: &mut impl WalletStorageWriteUnlocked,
        token_info: &UnconfirmedTokenInfo,
        median_time: BlockTimestamp,
        fee_rate: CurrentFeeRate,
    ) -> WalletResult<SignedTransaction> {
        let token_id = *token_info.token_id();
        token_info.check_can_lock()?;

        let nonce = token_info.get_next_nonce()?;
        let tx_input = TxInput::AccountCommand(nonce, AccountCommand::LockTokenSupply(token_id));
        let authority = token_info.authority()?.clone();

        self.change_token_supply_transaction(
            authority,
            tx_input,
            vec![],
            db_tx,
            median_time,
            fee_rate,
        )
    }

    pub fn freeze_token(
        &mut self,
        db_tx: &mut impl WalletStorageWriteUnlocked,
        token_info: &UnconfirmedTokenInfo,
        is_token_unfreezable: IsTokenUnfreezable,
        median_time: BlockTimestamp,
        fee_rate: CurrentFeeRate,
    ) -> WalletResult<SignedTransaction> {
        token_info.check_can_freeze()?;

        let nonce = token_info.get_next_nonce()?;
        let tx_input = TxInput::AccountCommand(
            nonce,
            AccountCommand::FreezeToken(*token_info.token_id(), is_token_unfreezable),
        );
        let authority = token_info.authority()?.clone();

        self.change_token_supply_transaction(
            authority,
            tx_input,
            vec![],
            db_tx,
            median_time,
            fee_rate,
        )
    }

    pub fn unfreeze_token(
        &mut self,
        db_tx: &mut impl WalletStorageWriteUnlocked,
        token_info: &UnconfirmedTokenInfo,
        median_time: BlockTimestamp,
        fee_rate: CurrentFeeRate,
    ) -> WalletResult<SignedTransaction> {
        token_info.check_can_unfreeze()?;

        let nonce = token_info.get_next_nonce()?;
        let tx_input =
            TxInput::AccountCommand(nonce, AccountCommand::UnfreezeToken(*token_info.token_id()));
        let authority = token_info.authority()?.clone();

        self.change_token_supply_transaction(
            authority,
            tx_input,
            vec![],
            db_tx,
            median_time,
            fee_rate,
        )
    }

    pub fn change_token_authority(
        &mut self,
        db_tx: &mut impl WalletStorageWriteUnlocked,
        token_info: &UnconfirmedTokenInfo,
        address: Address<Destination>,
        median_time: BlockTimestamp,
        fee_rate: CurrentFeeRate,
    ) -> WalletResult<SignedTransaction> {
        let new_authority = address.decode_object(&self.chain_config)?;

        let nonce = token_info.get_next_nonce()?;
        let tx_input = TxInput::AccountCommand(
            nonce,
            AccountCommand::ChangeTokenAuthority(*token_info.token_id(), new_authority),
        );
        let authority = token_info.authority()?.clone();

        self.change_token_supply_transaction(
            authority,
            tx_input,
            vec![],
            db_tx,
            median_time,
            fee_rate,
        )
    }

    fn change_token_supply_transaction(
        &mut self,
        authority: Destination,
        tx_input: TxInput,
        outputs: Vec<TxOutput>,
        db_tx: &mut impl WalletStorageWriteUnlocked,
        median_time: BlockTimestamp,
        fee_rate: CurrentFeeRate,
    ) -> Result<SignedTransaction, WalletError> {
        let request = SendRequest::new()
            .with_outputs(outputs)
            .with_inputs_and_destinations([(tx_input, authority)]);

        let request = self.select_inputs_for_send_request(
            request,
            vec![],
            db_tx,
            median_time,
            fee_rate.current_fee_rate,
            fee_rate.consolidate_fee_rate,
        )?;

        let tx = self.sign_transaction_from_req(request, db_tx)?;
        Ok(tx)
    }

    pub fn get_pos_gen_block_data(
        &self,
        db_tx: &impl WalletStorageReadUnlocked,
        pool_id: PoolId,
    ) -> WalletResult<PoSGenerateBlockInputData> {
        let pool_data = self.output_cache.pool_data(pool_id)?;
        let kernel_input: TxInput = pool_data.utxo_outpoint.clone().into();
        let stake_destination = &pool_data.stake_destination;
        let kernel_input_utxo =
            self.output_cache.get_txo(&pool_data.utxo_outpoint).expect("must exist");

        let stake_private_key = self
            .key_chain
            .get_private_key_for_destination(stake_destination, db_tx)?
            .ok_or(WalletError::KeyChainError(KeyChainError::NoPrivateKeyFound))?
            .private_key();

        let vrf_private_key = self
            .key_chain
            .get_vrf_private_key_for_public_key(&pool_data.vrf_public_key, db_tx)?
            .ok_or(WalletError::KeyChainError(KeyChainError::NoPrivateKeyFound))?
            .private_key();

        let data = PoSGenerateBlockInputData::new(
            stake_private_key,
            vrf_private_key,
            pool_id,
            vec![kernel_input],
            vec![kernel_input_utxo.clone()],
        );

        Ok(data)
    }

    fn sign_transaction_from_req(
        &self,
        request: SendRequest,
        db_tx: &impl WalletStorageReadUnlocked,
    ) -> WalletResult<SignedTransaction> {
        let (tx, input_utxos, destinations) = request.into_transaction_and_utxos()?;
        let destinations = destinations.iter().collect_vec();
        let input_utxos = input_utxos.iter().map(Option::as_ref).collect_vec();

        self.sign_transaction(tx, destinations.as_slice(), input_utxos.as_slice(), db_tx)?
            .into_signed_tx()
    }

    fn sign_input(
        &self,
        tx: &Transaction,
        destination: &Destination,
        input_index: usize,
        input_utxos: &[Option<&TxOutput>],
        db_tx: &impl WalletStorageReadUnlocked,
    ) -> WalletResult<Option<InputWitness>> {
        if *destination == Destination::AnyoneCanSpend {
            Ok(Some(InputWitness::NoSignature(None)))
        } else {
            self.key_chain
                .get_private_key_for_destination(destination, db_tx)?
                .map(|pk_from_keychain| {
                    let private_key = pk_from_keychain.private_key();
                    let sighash_type =
                        SigHashType::try_from(SigHashType::ALL).expect("Should not fail");

                    StandardInputSignature::produce_uniparty_signature_for_input(
                        &private_key,
                        sighash_type,
                        destination.clone(),
                        tx,
                        input_utxos,
                        input_index,
                    )
                    .map(InputWitness::Standard)
                    .map_err(WalletError::TransactionSig)
                })
                .transpose()
        }
    }

    fn sign_transaction(
        &self,
        tx: Transaction,
        destinations: &[&Destination],
        input_utxos: &[Option<&TxOutput>],
        db_tx: &impl WalletStorageReadUnlocked,
    ) -> WalletResult<PartiallySignedTransaction> {
        let witnesses = destinations
            .iter()
            .copied()
            .enumerate()
            .map(|(i, destination)| self.sign_input(&tx, destination, i, input_utxos, db_tx))
            .collect::<Result<Vec<Option<InputWitness>>, _>>()?;

        Ok(PartiallySignedTransaction::new(tx, witnesses))
    }

    pub fn sign_raw_transaction(
        &self,
        tx: PartiallySignedTransaction,
        db_tx: &impl WalletStorageReadUnlocked,
    ) -> WalletResult<PartiallySignedTransaction> {
        let (tx, witnesses) = tx.take();

        let input_utxos = tx
            .inputs()
            .iter()
            .map(|input| match input {
                TxInput::Utxo(outpoint) => Ok(Some(
                    self.output_cache.get_txo(outpoint).ok_or(WalletError::NoUtxos)?,
                )),
                TxInput::Account(_) | TxInput::AccountCommand(_, _) => {
                    Err(WalletError::InputCannotBeSigned)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        ensure!(
            input_utxos.len() == witnesses.len(),
            TransactionCreationError::InvalidWitnessCount
        );

        let witnesses = witnesses
            .into_iter()
            .enumerate()
            .map(|(i, witness)| match witness {
                Some(w) => Ok(Some(w)),
                None => match get_tx_output_destination(input_utxos[i].expect("cannot be none")) {
                    Some(destination) => {
                        let s = self
                            .sign_input(&tx, destination, i, &input_utxos, db_tx)?
                            .ok_or(WalletError::InputCannotBeSigned)?;
                        Ok(Some(s))
                    }
                    None => Ok(None),
                },
            })
            .collect::<Result<Vec<_>, WalletError>>()?;

        Ok(PartiallySignedTransaction::new(tx, witnesses))
    }

    pub fn account_index(&self) -> U31 {
        self.key_chain.account_index()
    }

    /// Get the id of this account
    pub fn get_account_id(&self) -> AccountId {
        self.key_chain.get_account_id()
    }

    /// Reload the keys from the DB
    /// Used to reset the in-memory state after a failed operation
    pub fn reload_keys(&mut self, db_tx: &impl WalletStorageReadLocked) -> WalletResult<()> {
        self.key_chain.reload_keys(db_tx)?;
        Ok(())
    }

    /// Get a new address that hasn't been used before
    pub fn get_new_address(
        &mut self,
        db_tx: &mut impl WalletStorageWriteLocked,
        purpose: KeyPurpose,
    ) -> WalletResult<(ChildNumber, Address<Destination>)> {
        Ok(self.key_chain.issue_address(db_tx, purpose)?)
    }

    /// Get a new public key that hasn't been used before
    pub fn get_new_public_key<B: storage::Backend>(
        &mut self,
        db_tx: &mut StoreTxRw<B>,
        purpose: KeyPurpose,
    ) -> WalletResult<PublicKey> {
        Ok(self.key_chain.issue_key(db_tx, purpose)?.into_public_key())
    }

    pub fn get_all_issued_addresses(&self) -> BTreeMap<ChildNumber, Address<Destination>> {
        self.key_chain.get_all_issued_addresses()
    }

    pub fn get_all_issued_vrf_public_keys(
        &self,
    ) -> BTreeMap<ChildNumber, (Address<VRFPublicKey>, bool)> {
        self.key_chain.get_all_issued_vrf_public_keys()
    }

    pub fn get_legacy_vrf_public_key(&self) -> Address<VRFPublicKey> {
        self.key_chain.get_legacy_vrf_public_key()
    }

    pub fn get_addresses_usage(&self) -> &KeychainUsageState {
        self.key_chain.get_addresses_usage_state()
    }

    /// Return true if this transaction output is can be spent by this account or if it is being
    /// watched.
    fn is_mine_or_watched(&self, txo: &TxOutput) -> bool {
        get_tx_output_destination(txo).map_or(false, |d| self.is_mine_or_watched_destination(d))
            || get_reward_output_destination(txo)
                .map_or(false, |d| self.is_mine_or_watched_destination(d))
    }

    /// Return true if this destination can be spent by this account or if it is being watched.
    fn is_mine_or_watched_destination(&self, destination: &Destination) -> bool {
        match destination {
            Destination::Address(pkh) => self.key_chain.is_public_key_hash_mine(pkh),
            Destination::PublicKey(pk) => self.key_chain.is_public_key_mine(pk),
            Destination::AnyoneCanSpend => false,
            Destination::ScriptHash(_) | Destination::ClassicMultisig(_) => false,
        }
    }

    fn mark_outputs_as_seen(
        &mut self,
        db_tx: &mut impl WalletStorageWriteLocked,
        outputs: &[TxOutput],
    ) -> WalletResult<bool> {
        let mut found = false;
        // Process all outputs (without short-circuiting)
        for output in outputs {
            found |= self.mark_output_as_seen(db_tx, output)?;
        }
        Ok(found)
    }

    fn mark_output_as_seen(
        &mut self,
        db_tx: &mut impl WalletStorageWriteLocked,
        output: &TxOutput,
    ) -> WalletResult<bool> {
        self.mark_created_stake_pool_as_seen(output, db_tx)?;

        if let Some(d) = get_tx_output_destination(output) {
            match d {
                Destination::Address(pkh) => {
                    let found = self.key_chain.mark_public_key_hash_as_used(db_tx, pkh)?;
                    if found {
                        return Ok(true);
                    }
                }
                Destination::PublicKey(pk) => {
                    let found = self.key_chain.mark_public_key_as_used(db_tx, pk)?;
                    if found {
                        return Ok(true);
                    }
                }
                Destination::AnyoneCanSpend => return Ok(false),
                Destination::ClassicMultisig(_) | Destination::ScriptHash(_) => {}
            }
        }
        Ok(false)
    }

    /// check if the output is a CreateStakePool and check if the VRF key or decommission_key
    /// are tracked by this wallet and mark them as used
    fn mark_created_stake_pool_as_seen(
        &mut self,
        output: &TxOutput,
        db_tx: &mut impl WalletStorageWriteLocked,
    ) -> Result<(), WalletError> {
        if let TxOutput::CreateStakePool(_, data) = output {
            self.key_chain.mark_vrf_public_key_as_used(db_tx, data.vrf_public_key())?;
            match data.decommission_key() {
                Destination::Address(pkh) => {
                    self.key_chain.mark_public_key_hash_as_used(db_tx, pkh)?;
                }
                Destination::PublicKey(pk) => {
                    self.key_chain.mark_public_key_as_used(db_tx, pk)?;
                }
                Destination::AnyoneCanSpend
                | Destination::ClassicMultisig(_)
                | Destination::ScriptHash(_) => {}
            }
        }
        Ok(())
    }

    pub fn get_balance(
        &self,
        utxo_types: UtxoTypes,
        utxo_states: UtxoStates,
        median_time: BlockTimestamp,
        with_locked: WithLocked,
    ) -> WalletResult<BTreeMap<Currency, Amount>> {
        let amounts_by_currency = group_utxos_for_input(
            self.get_utxos(utxo_types, median_time, utxo_states, with_locked).into_iter(),
            |(_, (tx_output, _))| tx_output,
            |total: &mut Amount, _, amount| -> WalletResult<()> {
                *total = (*total + amount).ok_or(WalletError::OutputAmountOverflow)?;
                Ok(())
            },
            Amount::ZERO,
        )?;
        Ok(amounts_by_currency)
    }

    pub fn get_utxos(
        &self,
        utxo_types: UtxoTypes,
        median_time: BlockTimestamp,
        utxo_states: UtxoStates,
        with_locked: WithLocked,
    ) -> Vec<(UtxoOutPoint, (&TxOutput, Option<TokenId>))> {
        let current_block_info = BlockInfo {
            height: self.account_info.best_block_height(),
            timestamp: median_time,
        };
        self.output_cache.utxos_with_token_ids(
            current_block_info,
            utxo_states,
            with_locked,
            |txo| {
                self.is_mine_or_watched(txo)
                    && get_utxo_type(txo).is_some_and(|v| utxo_types.contains(v))
            },
        )
    }

    pub fn get_transaction_list(&self, skip: usize, count: usize) -> WalletResult<TransactionList> {
        get_transaction_list(&self.key_chain, &self.output_cache, skip, count)
    }

    pub fn get_transaction(&self, transaction_id: Id<Transaction>) -> WalletResult<&TxData> {
        self.output_cache.get_transaction(transaction_id)
    }

    pub fn reset_to_height<B: storage::Backend>(
        &mut self,
        db_tx: &mut StoreTxRw<B>,
        wallet_events: &impl WalletEvents,
        common_block_height: BlockHeight,
    ) -> WalletResult<()> {
        let mut revoked_txs = self
            .output_cache
            .txs_with_unconfirmed()
            .iter()
            .filter_map(|(id, tx)| match tx.state() {
                TxState::Confirmed(height, _, idx) => {
                    if height > common_block_height {
                        Some((
                            AccountWalletTxId::new(self.get_account_id(), id.clone()),
                            (height, idx),
                        ))
                    } else {
                        None
                    }
                }
                TxState::Inactive(_)
                | TxState::Conflicted(_)
                | TxState::InMempool(_)
                | TxState::Abandoned => None,
            })
            .collect::<Vec<_>>();

        // sort from latest tx down to remove them in order
        revoked_txs.sort_by_key(|&(_, height_idx)| Reverse(height_idx));

        for (tx_id, _) in revoked_txs {
            db_tx.del_transaction(&tx_id)?;
            wallet_events.del_transaction(&tx_id);
            self.output_cache.remove_tx(&tx_id.into_item_id())?;
        }

        Ok(())
    }

    /// Store a block or tx in the DB if any of the inputs or outputs belong to this wallet
    /// returns true if tx was added false otherwise
    fn add_wallet_tx_if_relevant(
        &mut self,
        db_tx: &mut impl WalletStorageWriteLocked,
        wallet_events: &impl WalletEvents,
        tx: WalletTx,
    ) -> WalletResult<bool> {
        let relevant_inputs = tx.inputs().iter().any(|input| match input {
            TxInput::Utxo(outpoint) => self
                .output_cache
                .get_txo(outpoint)
                .map_or(false, |txo| self.is_mine_or_watched(txo)),
            TxInput::Account(outpoint) => match outpoint.account() {
                AccountSpending::DelegationBalance(delegation_id, _) => {
                    self.find_delegation(delegation_id).is_ok()
                }
            },
            TxInput::AccountCommand(_, op) => match op {
                AccountCommand::MintTokens(token_id, _)
                | AccountCommand::UnmintTokens(token_id)
                | AccountCommand::LockTokenSupply(token_id)
                | AccountCommand::FreezeToken(token_id, _)
                | AccountCommand::UnfreezeToken(token_id) => self.find_token(token_id).is_ok(),
                AccountCommand::ChangeTokenAuthority(token_id, address) => {
                    self.find_token(token_id).is_ok()
                        || self.is_mine_or_watched_destination(address)
                }
            },
        });
        let relevant_outputs = self.mark_outputs_as_seen(db_tx, tx.outputs())?;
        if relevant_inputs || relevant_outputs {
            let id = AccountWalletTxId::new(self.get_account_id(), tx.id());
            db_tx.set_transaction(&id, &tx)?;
            wallet_events.set_transaction(&id, &tx);
            self.output_cache.add_tx(id.into_item_id(), tx)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn scan_genesis(
        &mut self,
        db_tx: &mut impl WalletStorageWriteLocked,
        wallet_events: &impl WalletEvents,
    ) -> WalletResult<()> {
        let chain_config = Arc::clone(&self.chain_config);

        let block = BlockData::from_genesis(chain_config.genesis_block());
        self.add_wallet_tx_if_relevant(db_tx, wallet_events, WalletTx::Block(block))?;

        Ok(())
    }

    /// Scan the new blocks for relevant transactions and updates the state
    /// Returns true if a new transaction was added else false
    pub fn scan_new_blocks<B: storage::Backend>(
        &mut self,
        db_tx: &mut StoreTxRw<B>,
        wallet_events: &impl WalletEvents,
        common_block_height: BlockHeight,
        blocks: &[Block],
    ) -> WalletResult<bool> {
        assert!(!blocks.is_empty());
        assert!(
            common_block_height <= self.account_info.best_block_height(),
            "Invalid common block height: {}, current block height: {}",
            common_block_height,
            self.account_info.best_block_height(),
        );

        if self.account_info.best_block_height() > common_block_height {
            self.reset_to_height(db_tx, wallet_events, common_block_height)?;
        }

        let new_tx_was_added = blocks.iter().enumerate().try_fold(
            false,
            |mut new_tx_was_added, (index, block)| -> WalletResult<bool> {
                let block_height =
                    BlockHeight::new(common_block_height.into_int() + index as u64 + 1);
                let wallet_tx = WalletTx::Block(BlockData::from_block(block, block_height));

                new_tx_was_added |=
                    self.add_wallet_tx_if_relevant(db_tx, wallet_events, wallet_tx)?;

                block.transactions().iter().enumerate().try_fold(
                    new_tx_was_added,
                    |mut new_tx_was_added, (idx, signed_tx)| {
                        let tx_state =
                            TxState::Confirmed(block_height, block.timestamp(), idx as u64);
                        let wallet_tx = WalletTx::Tx(TxData::new(signed_tx.clone(), tx_state));
                        self.update_conflicting_txs(&wallet_tx, block, db_tx)?;

                        new_tx_was_added |= self
                            .add_wallet_tx_if_relevant_and_remove_from_user_txs(
                                db_tx,
                                wallet_events,
                                wallet_tx,
                                signed_tx.transaction().get_id(),
                            )?;
                        Ok(new_tx_was_added)
                    },
                )
            },
        )?;

        // Update best_block_height and best_block_id only after successful commit call!
        let best_block_height = (common_block_height.into_int() + blocks.len() as u64).into();
        let best_block_id = blocks.last().expect("blocks not empty").header().block_id().into();

        self.account_info.update_best_block(best_block_height, best_block_id);
        db_tx.set_account(&self.key_chain.get_account_id(), &self.account_info)?;

        Ok(new_tx_was_added)
    }

    /// Check for any conflicting txs and update the new state in the DB
    fn update_conflicting_txs<B: storage::Backend>(
        &mut self,
        wallet_tx: &WalletTx,
        block: &Block,
        db_tx: &mut StoreTxRw<B>,
    ) -> WalletResult<()> {
        let acc_id = self.get_account_id();
        let conflicting_tx = self.output_cache.check_conflicting(wallet_tx, block.get_id().into());
        for tx in conflicting_tx {
            let id = AccountWalletTxId::new(acc_id.clone(), tx.id());
            db_tx.set_transaction(&id, tx)?;
        }

        Ok(())
    }

    /// Add a new wallet tx if relevant for this account and remove it from the user transactions
    /// to not be rebroadcast again
    fn add_wallet_tx_if_relevant_and_remove_from_user_txs(
        &mut self,
        db_tx: &mut impl WalletStorageWriteLocked,
        wallet_events: &impl WalletEvents,
        wallet_tx: WalletTx,
        tx_id: Id<Transaction>,
    ) -> Result<bool, WalletError> {
        Ok(
            if self.add_wallet_tx_if_relevant(db_tx, wallet_events, wallet_tx)? {
                let id = AccountWalletCreatedTxId::new(self.get_account_id(), tx_id);
                db_tx.del_user_transaction(&id)?;
                true
            } else {
                false
            },
        )
    }

    pub fn update_best_block(
        &mut self,
        db_tx: &mut impl WalletStorageWriteLocked,
        best_block_height: BlockHeight,
        best_block_id: Id<GenBlock>,
    ) -> WalletResult<()> {
        self.account_info.update_best_block(best_block_height, best_block_id);
        db_tx.set_account(&self.key_chain.get_account_id(), &self.account_info)?;
        Ok(())
    }

    pub fn scan_new_inmempool_transactions(
        &mut self,
        transactions: &[SignedTransaction],
        db_tx: &mut impl WalletStorageWriteLocked,
        wallet_events: &impl WalletEvents,
    ) -> WalletResult<()> {
        self.scan_new_unconfirmed_transactions(
            transactions,
            TxState::InMempool,
            db_tx,
            wallet_events,
        )
    }

    pub fn scan_new_inactive_transactions(
        &mut self,
        transactions: &[SignedTransaction],
        db_tx: &mut impl WalletStorageWriteLocked,
        wallet_events: &impl WalletEvents,
    ) -> WalletResult<()> {
        self.scan_new_unconfirmed_transactions(
            transactions,
            TxState::Inactive,
            db_tx,
            wallet_events,
        )
    }

    fn scan_new_unconfirmed_transactions(
        &mut self,
        transactions: &[SignedTransaction],
        make_tx_state: fn(u64) -> TxState,
        db_tx: &mut impl WalletStorageWriteLocked,
        wallet_events: &impl WalletEvents,
    ) -> WalletResult<()> {
        let account_id = self.get_account_id();
        let mut not_added = vec![];
        let mut counter = db_tx
            .get_account_unconfirmed_tx_counter(&account_id)?
            .ok_or(WalletError::WalletNotInitialized)?;

        for signed_tx in transactions {
            counter += 1;
            let tx_state = make_tx_state(counter);
            let wallet_tx = WalletTx::Tx(TxData::new(signed_tx.clone(), tx_state));

            if !self.add_wallet_tx_if_relevant(db_tx, wallet_events, wallet_tx)? {
                not_added.push((signed_tx, tx_state));
            } else {
                let id =
                    AccountPrefixedId::new(account_id.clone(), signed_tx.transaction().get_id());
                db_tx.set_user_transaction(&id, signed_tx)?;
            }
        }

        // check them again after adding all we could
        // and keep looping as long as we add a new tx
        loop {
            let mut not_added_next = vec![];
            let previously_not_added = not_added.len();
            for (signed_tx, tx_state) in not_added {
                let wallet_tx = WalletTx::Tx(TxData::new(signed_tx.clone(), tx_state));

                if !self.add_wallet_tx_if_relevant(db_tx, wallet_events, wallet_tx)? {
                    not_added_next.push((signed_tx, tx_state));
                } else {
                    let id = AccountPrefixedId::new(
                        account_id.clone(),
                        signed_tx.transaction().get_id(),
                    );
                    db_tx.set_user_transaction(&id, signed_tx)?;
                }
            }

            // if no new tx was added break
            if not_added_next.len() == previously_not_added {
                break;
            }

            not_added = not_added_next;
        }

        // update the new counter in the DB
        db_tx.set_account_unconfirmed_tx_counter(&self.get_account_id(), counter)?;

        Ok(())
    }

    pub fn best_block(&self) -> (Id<GenBlock>, BlockHeight) {
        (
            self.account_info.best_block_id(),
            self.account_info.best_block_height(),
        )
    }

    pub fn has_transactions(&self) -> bool {
        self.output_cache.has_confirmed_transactions()
    }

    pub fn name(&self) -> &Option<String> {
        self.account_info.name()
    }

    pub fn pending_transactions(&self) -> Vec<WithId<&Transaction>> {
        self.output_cache.pending_transactions()
    }

    pub fn abandon_transaction(
        &mut self,
        tx_id: Id<Transaction>,
        db_tx: &mut impl WalletStorageWriteLocked,
    ) -> WalletResult<()> {
        let abandoned_txs = self.output_cache.abandon_transaction(tx_id)?;
        let acc_id = self.get_account_id();

        for tx_id in abandoned_txs {
            let id = AccountWalletCreatedTxId::new(acc_id.clone(), tx_id);
            db_tx.del_user_transaction(&id)?;
        }

        Ok(())
    }

    pub fn set_name(
        &mut self,
        name: Option<String>,
        db_tx: &mut impl WalletStorageWriteLocked,
    ) -> WalletResult<()> {
        self.account_info.set_name(name);
        db_tx.set_account(&self.get_account_id(), &self.account_info)?;
        Ok(())
    }

    pub fn get_created_blocks(&self) -> Vec<Id<GenBlock>> {
        self.output_cache
            .get_created_blocks(|destination| self.is_mine_or_watched_destination(destination))
    }

    pub fn top_up_addresses(
        &mut self,
        db_tx: &mut impl WalletStorageWriteLocked,
    ) -> WalletResult<()> {
        self.key_chain.top_up_all(db_tx)?;
        Ok(())
    }
}

/// There are some preselected inputs like the Token account inputs with a nonce
/// that need to be included in the request
/// Here we group them up by currency and sum the total amount and fee they bring to the
/// transaction
fn group_preselected_inputs(
    request: &SendRequest,
    current_fee_rate: FeeRate,
    chain_config: &ChainConfig,
) -> Result<BTreeMap<Currency, (Amount, Amount)>, WalletError> {
    let mut preselected_inputs = BTreeMap::new();
    for (input, destination) in request.inputs().iter().zip(request.destinations()) {
        let input_size = serialization::Encode::encoded_size(&input);
        let inp_sig_size = input_signature_size_from_destination(destination)?;

        let fee = current_fee_rate
            .compute_fee(input_size + inp_sig_size)
            .map_err(|_| UtxoSelectorError::AmountArithmeticError)?;

        let mut update_preselected_inputs =
            |currency: Currency, amount: Amount, fee: Amount| -> WalletResult<()> {
                match preselected_inputs.entry(currency) {
                    Entry::Vacant(entry) => {
                        entry.insert((amount, fee));
                    }
                    Entry::Occupied(mut entry) => {
                        let (existing_amount, existing_fee) = entry.get_mut();
                        *existing_amount =
                            (*existing_amount + amount).ok_or(WalletError::OutputAmountOverflow)?;
                        *existing_fee =
                            (*existing_fee + fee).ok_or(WalletError::OutputAmountOverflow)?;
                    }
                }
                Ok(())
            };

        match input {
            TxInput::Utxo(_) => {}
            TxInput::Account(outpoint) => match outpoint.account() {
                AccountSpending::DelegationBalance(_, amount) => {
                    update_preselected_inputs(Currency::Coin, *amount, *fee)?;
                }
            },
            TxInput::AccountCommand(_, op) => match op {
                AccountCommand::MintTokens(token_id, amount) => {
                    update_preselected_inputs(
                        Currency::Token(*token_id),
                        *amount,
                        (*fee + chain_config.token_supply_change_fee())
                            .ok_or(WalletError::OutputAmountOverflow)?,
                    )?;
                }
                AccountCommand::LockTokenSupply(token_id)
                | AccountCommand::UnmintTokens(token_id) => {
                    update_preselected_inputs(
                        Currency::Token(*token_id),
                        Amount::ZERO,
                        (*fee + chain_config.token_supply_change_fee())
                            .ok_or(WalletError::OutputAmountOverflow)?,
                    )?;
                }
                AccountCommand::FreezeToken(token_id, _)
                | AccountCommand::UnfreezeToken(token_id) => {
                    update_preselected_inputs(
                        Currency::Token(*token_id),
                        Amount::ZERO,
                        (*fee + chain_config.token_freeze_fee())
                            .ok_or(WalletError::OutputAmountOverflow)?,
                    )?;
                }
                AccountCommand::ChangeTokenAuthority(token_id, _) => {
                    update_preselected_inputs(
                        Currency::Token(*token_id),
                        Amount::ZERO,
                        (*fee + chain_config.token_change_authority_fee())
                            .ok_or(WalletError::OutputAmountOverflow)?,
                    )?;
                }
            },
        }
    }
    Ok(preselected_inputs)
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum Currency {
    Coin,
    Token(TokenId),
}

fn group_outputs<T, Grouped: Clone>(
    outputs: impl Iterator<Item = T>,
    get_tx_output: impl Fn(&T) -> &TxOutput,
    mut combiner: impl FnMut(&mut Grouped, &T, Amount) -> WalletResult<()>,
    init: Grouped,
) -> WalletResult<BTreeMap<Currency, Grouped>> {
    let mut coin_grouped = init.clone();
    let mut tokens_grouped: BTreeMap<Currency, Grouped> = BTreeMap::new();

    // Iterate over all outputs and group them up by currency
    for output in outputs {
        // Get the supported output value
        let output_value = match get_tx_output(&output) {
            TxOutput::Transfer(v, _) | TxOutput::LockThenTransfer(v, _, _) | TxOutput::Burn(v) => {
                v.clone()
            }
            TxOutput::CreateStakePool(_, stake) => OutputValue::Coin(stake.value()),
            TxOutput::DelegateStaking(amount, _) => OutputValue::Coin(*amount),
            TxOutput::CreateDelegationId(_, _)
            | TxOutput::IssueFungibleToken(_)
            | TxOutput::IssueNft(_, _, _)
            | TxOutput::DataDeposit(_) => continue,
            TxOutput::ProduceBlockFromStake(_, _) => {
                return Err(WalletError::UnsupportedTransactionOutput(Box::new(
                    get_tx_output(&output).clone(),
                )))
            }
        };

        match output_value {
            OutputValue::Coin(output_amount) => {
                combiner(&mut coin_grouped, &output, output_amount)?;
            }
            OutputValue::TokenV0(_) => { /* ignore */ }
            OutputValue::TokenV1(id, amount) => {
                let total_token_amount =
                    tokens_grouped.entry(Currency::Token(id)).or_insert_with(|| init.clone());

                combiner(total_token_amount, &output, amount)?;
            }
        }
    }

    tokens_grouped.insert(Currency::Coin, coin_grouped);
    Ok(tokens_grouped)
}

fn group_outputs_with_issuance_fee<T, Grouped: Clone>(
    outputs: impl Iterator<Item = T>,
    get_tx_output: impl Fn(&T) -> &TxOutput,
    mut combiner: impl FnMut(&mut Grouped, &T, Amount) -> WalletResult<()>,
    init: Grouped,
    chain_config: &ChainConfig,
) -> WalletResult<BTreeMap<Currency, Grouped>> {
    let mut coin_grouped = init.clone();
    let mut tokens_grouped: BTreeMap<Currency, Grouped> = BTreeMap::new();

    // Iterate over all outputs and group them up by currency
    for output in outputs {
        // Get the supported output value
        let output_value = match get_tx_output(&output) {
            TxOutput::Transfer(v, _) | TxOutput::LockThenTransfer(v, _, _) | TxOutput::Burn(v) => {
                v.clone()
            }
            TxOutput::CreateStakePool(_, stake) => OutputValue::Coin(stake.value()),
            TxOutput::DelegateStaking(amount, _) => OutputValue::Coin(*amount),
            TxOutput::IssueFungibleToken(_) => {
                OutputValue::Coin(chain_config.fungible_token_issuance_fee())
            }
            TxOutput::IssueNft(_, _, _) => OutputValue::Coin(chain_config.nft_issuance_fee()),
            TxOutput::DataDeposit(_) => OutputValue::Coin(chain_config.data_deposit_fee()),
            TxOutput::CreateDelegationId(_, _) => continue,
            TxOutput::ProduceBlockFromStake(_, _) => {
                return Err(WalletError::UnsupportedTransactionOutput(Box::new(
                    get_tx_output(&output).clone(),
                )))
            }
        };

        match output_value {
            OutputValue::Coin(output_amount) => {
                combiner(&mut coin_grouped, &output, output_amount)?;
            }
            OutputValue::TokenV0(_) => { /* ignore */ }
            OutputValue::TokenV1(id, amount) => {
                let total_token_amount =
                    tokens_grouped.entry(Currency::Token(id)).or_insert_with(|| init.clone());

                combiner(total_token_amount, &output, amount)?;
            }
        }
    }

    tokens_grouped.insert(Currency::Coin, coin_grouped);
    Ok(tokens_grouped)
}

fn group_utxos_for_input<T, Grouped: Clone>(
    outputs: impl Iterator<Item = T>,
    get_tx_output: impl Fn(&T) -> &TxOutput,
    mut combiner: impl FnMut(&mut Grouped, &T, Amount) -> WalletResult<()>,
    init: Grouped,
) -> WalletResult<BTreeMap<Currency, Grouped>> {
    let mut coin_grouped = init.clone();
    let mut tokens_grouped: BTreeMap<Currency, Grouped> = BTreeMap::new();

    // Iterate over all outputs and group them up by currency
    for output in outputs {
        // Get the supported output value
        let output_value = match get_tx_output(&output) {
            TxOutput::Transfer(v, _) | TxOutput::LockThenTransfer(v, _, _) => v.clone(),
            TxOutput::CreateStakePool(_, stake) => OutputValue::Coin(stake.value()),
            TxOutput::IssueNft(token_id, _, _) => {
                OutputValue::TokenV1(*token_id, Amount::from_atoms(1))
            }
            TxOutput::ProduceBlockFromStake(_, _)
            | TxOutput::Burn(_)
            | TxOutput::CreateDelegationId(_, _)
            | TxOutput::DelegateStaking(_, _)
            | TxOutput::IssueFungibleToken(_)
            | TxOutput::DataDeposit(_) => {
                return Err(WalletError::UnsupportedTransactionOutput(Box::new(
                    get_tx_output(&output).clone(),
                )))
            }
        };

        match output_value {
            OutputValue::Coin(output_amount) => {
                combiner(&mut coin_grouped, &output, output_amount)?;
            }
            OutputValue::TokenV0(_) => { /* ignore */ }
            OutputValue::TokenV1(id, amount) => {
                let total_token_amount =
                    tokens_grouped.entry(Currency::Token(id)).or_insert_with(|| init.clone());

                combiner(total_token_amount, &output, amount)?;
            }
        }
    }

    tokens_grouped.insert(Currency::Coin, coin_grouped);
    Ok(tokens_grouped)
}

/// Calculate the amount of fee that needs to be paid to add a change output
/// Returns the Amounts for Coin output and Token output
fn coin_and_token_output_change_fees(feerate: mempool::FeeRate) -> WalletResult<(Amount, Amount)> {
    let pub_key_hash = PublicKeyHash::from_low_u64_ne(0);

    let destination = Destination::Address(pub_key_hash);

    let coin_output = TxOutput::Transfer(OutputValue::Coin(Amount::MAX), destination.clone());
    let token_output = TxOutput::Transfer(
        OutputValue::TokenV1(
            TokenId::zero(),
            // TODO: as the  amount is compact there is an edge case where those extra few bytes of
            // size can cause the output fee to be go over the available amount of coins thus not
            // including a change output, and losing money for the user
            // e.g. available money X and need to transfer Y and the difference Z = X - Y is just
            // enough the make an output with change but the amount having single byte encoding
            // but by using Amount::MAX the algorithm thinks that the change output will cost more
            // than Z and it will not create a change output
            Amount::MAX,
        ),
        destination,
    );

    Ok((
        feerate
            .compute_fee(serialization::Encode::encoded_size(&coin_output))
            .map_err(|_| UtxoSelectorError::AmountArithmeticError)?
            .into(),
        feerate
            .compute_fee(serialization::Encode::encoded_size(&token_output))
            .map_err(|_| UtxoSelectorError::AmountArithmeticError)?
            .into(),
    ))
}

#[cfg(test)]
mod tests;
