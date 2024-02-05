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

use std::{collections::BTreeMap, num::NonZeroU64};

use common::{
    chain::{
        output_value::OutputValue, timelock::OutputTimeLock, AccountCommand, AccountSpending,
        AccountsBalancesCheckVersion, ChainConfig, DelegationId, PoolId, TxInput, TxOutput,
        UtxoOutPoint,
    },
    primitives::{Amount, BlockHeight, CoinOrTokenId, Fee, Subsidy},
};
use utils::ensure;

use crate::accounts_balances_tracker::AccountsBalancesTracker;

use super::{accumulated_fee::AccumulatedFee, insert_or_increase, Error};

/// `ConstrainedValueAccumulator` helps avoiding messy inputs/outputs combinations analysis by
/// providing a set of properties that should be satisfied. For example instead of checking that
/// all outputs are timelocked when the pool is decommissioned `ConstrainedValueAccumulator` gives a way
/// to check that an accumulated output value is locked for sufficient amount of time which allows
/// using other valid inputs and outputs in the same tx.
#[derive(Debug, PartialEq, Eq)]
pub struct ConstrainedValueAccumulator {
    unconstrained_value: BTreeMap<CoinOrTokenId, Amount>,
    timelock_constrained: BTreeMap<NonZeroU64, Amount>,
}

impl ConstrainedValueAccumulator {
    pub fn new() -> Self {
        Self {
            unconstrained_value: Default::default(),
            timelock_constrained: Default::default(),
        }
    }

    pub fn from_block_reward(total_fee: Fee, subsidy: Subsidy) -> Option<Self> {
        let initial_value = (total_fee.0 + subsidy.0)?;
        Some(Self {
            unconstrained_value: BTreeMap::from_iter([(CoinOrTokenId::Coin, initial_value)]),
            timelock_constrained: Default::default(),
        })
    }

    pub fn from_inputs<StakerBalanceGetterFn, DelegationBalanceGetterFn>(
        chain_config: &ChainConfig,
        block_height: BlockHeight,
        staker_balance_getter: StakerBalanceGetterFn,
        delegation_balance_getter: DelegationBalanceGetterFn,
        inputs: &[TxInput],
        inputs_utxos: &[Option<TxOutput>],
    ) -> Result<Self, Error>
    where
        StakerBalanceGetterFn: Fn(PoolId) -> Result<Option<Amount>, Error>,
        DelegationBalanceGetterFn: Fn(DelegationId) -> Result<Option<Amount>, Error>,
    {
        ensure!(
            inputs.len() == inputs_utxos.len(),
            Error::InputsAndInputsUtxosLengthMismatch(inputs.len(), inputs_utxos.len())
        );

        let mut accumulator = Self::new();
        let mut total_fee_deducted = Amount::ZERO;
        let mut accounts_balances_tracker =
            AccountsBalancesTracker::new(&delegation_balance_getter);

        for (input, input_utxo) in inputs.iter().zip(inputs_utxos.iter()) {
            match input {
                TxInput::Utxo(outpoint) => {
                    let input_utxo =
                        input_utxo.as_ref().ok_or(Error::MissingOutputOrSpent(outpoint.clone()))?;
                    accumulator.process_input_utxo(
                        chain_config,
                        block_height,
                        &staker_balance_getter,
                        outpoint.clone(),
                        input_utxo,
                    )?;
                }
                TxInput::Account(outpoint) => {
                    accumulator.process_input_account(
                        chain_config,
                        block_height,
                        outpoint.account(),
                        &mut accounts_balances_tracker,
                        &delegation_balance_getter,
                    )?;
                }
                TxInput::AccountCommand(_, command) => {
                    let fee_to_deduct = accumulator.process_input_account_command(
                        chain_config,
                        block_height,
                        command,
                    )?;

                    total_fee_deducted = (total_fee_deducted + fee_to_deduct)
                        .ok_or(Error::CoinOrTokenOverflow(CoinOrTokenId::Coin))?;
                }
            }
        }

        decrease_or(
            &mut accumulator.unconstrained_value,
            CoinOrTokenId::Coin,
            total_fee_deducted,
            Error::AttemptToViolateFeeRequirements,
        )?;

        Ok(accumulator)
    }

    fn process_input_utxo<StakerBalanceGetterFn>(
        &mut self,
        chain_config: &ChainConfig,
        block_height: BlockHeight,
        staker_balance_getter: &StakerBalanceGetterFn,
        outpoint: UtxoOutPoint,
        input_utxo: &TxOutput,
    ) -> Result<(), Error>
    where
        StakerBalanceGetterFn: Fn(PoolId) -> Result<Option<Amount>, Error>,
    {
        match input_utxo {
            TxOutput::Transfer(value, _) | TxOutput::LockThenTransfer(value, _, _) => {
                match value {
                    OutputValue::Coin(amount) => insert_or_increase(
                        &mut self.unconstrained_value,
                        CoinOrTokenId::Coin,
                        *amount,
                    )?,
                    OutputValue::TokenV0(_) => { /* ignore */ }
                    OutputValue::TokenV1(token_id, amount) => insert_or_increase(
                        &mut self.unconstrained_value,
                        CoinOrTokenId::TokenId(*token_id),
                        *amount,
                    )?,
                };
            }
            TxOutput::CreateDelegationId(..)
            | TxOutput::IssueFungibleToken(..)
            | TxOutput::Burn(_)
            | TxOutput::DataDeposit(_) => {
                return Err(Error::SpendingNonSpendableOutput(outpoint.clone()));
            }
            TxOutput::IssueNft(token_id, _, _) => {
                insert_or_increase(
                    &mut self.unconstrained_value,
                    CoinOrTokenId::TokenId(*token_id),
                    Amount::from_atoms(1),
                )?;
            }
            TxOutput::DelegateStaking(coins, _) => {
                insert_or_increase(&mut self.unconstrained_value, CoinOrTokenId::Coin, *coins)?;
            }
            TxOutput::CreateStakePool(pool_id, _) | TxOutput::ProduceBlockFromStake(_, pool_id) => {
                let staker_balance = staker_balance_getter(*pool_id)?
                    .ok_or(Error::PledgeAmountNotFound(*pool_id))?;
                let maturity_distance =
                    chain_config.staking_pool_spend_maturity_block_count(block_height);

                match NonZeroU64::new(maturity_distance.to_int()) {
                    Some(maturity_distance) => {
                        let balance = self
                            .timelock_constrained
                            .entry(maturity_distance)
                            .or_insert(Amount::ZERO);
                        *balance = (*balance + staker_balance)
                            .ok_or(Error::CoinOrTokenOverflow(CoinOrTokenId::Coin))?;
                    }
                    None => {
                        insert_or_increase(
                            &mut self.unconstrained_value,
                            CoinOrTokenId::Coin,
                            staker_balance,
                        )?;
                    }
                }
            }
        };

        Ok(())
    }

    fn process_input_account<DelegationBalanceGetterFn>(
        &mut self,
        chain_config: &ChainConfig,
        block_height: BlockHeight,
        account: &AccountSpending,
        accounts_balances_tracker: &mut AccountsBalancesTracker<DelegationBalanceGetterFn>,
        delegation_balance_getter: &DelegationBalanceGetterFn,
    ) -> Result<(), Error>
    where
        DelegationBalanceGetterFn: Fn(DelegationId) -> Result<Option<Amount>, Error>,
    {
        match account {
            AccountSpending::DelegationBalance(delegation_id, spend_amount) => {
                match chain_config
                    .chainstate_upgrades()
                    .version_at_height(block_height)
                    .1
                    .accounts_balances_version()
                {
                    AccountsBalancesCheckVersion::V0 => {
                        let delegation_balance = delegation_balance_getter(*delegation_id)?
                            .ok_or(Error::DelegationBalanceNotFound(*delegation_id))?;
                        ensure!(
                            *spend_amount <= delegation_balance,
                            Error::AttemptToPrintMoney(CoinOrTokenId::Coin)
                        );
                    }
                    AccountsBalancesCheckVersion::V1 => {
                        accounts_balances_tracker.spend_from_account(account.clone())?;
                    }
                }

                let maturity_distance =
                    chain_config.staking_pool_spend_maturity_block_count(block_height);

                match NonZeroU64::new(maturity_distance.to_int()) {
                    Some(maturity_distance) => {
                        let balance = self
                            .timelock_constrained
                            .entry(maturity_distance)
                            .or_insert(Amount::ZERO);
                        *balance = (*balance + *spend_amount)
                            .ok_or(Error::CoinOrTokenOverflow(CoinOrTokenId::Coin))?;
                    }
                    None => {
                        insert_or_increase(
                            &mut self.unconstrained_value,
                            CoinOrTokenId::Coin,
                            *spend_amount,
                        )?;
                    }
                }
            }
        };

        Ok(())
    }

    fn process_input_account_command(
        &mut self,
        chain_config: &ChainConfig,
        block_height: BlockHeight,
        command: &AccountCommand,
    ) -> Result<Amount, Error> {
        match command {
            AccountCommand::MintTokens(token_id, amount) => {
                insert_or_increase(
                    &mut self.unconstrained_value,
                    CoinOrTokenId::TokenId(*token_id),
                    *amount,
                )?;
                Ok(chain_config.token_supply_change_fee(block_height))
            }
            AccountCommand::LockTokenSupply(_) | AccountCommand::UnmintTokens(_) => {
                Ok(chain_config.token_supply_change_fee(block_height))
            }
            AccountCommand::FreezeToken(_, _) | AccountCommand::UnfreezeToken(_) => {
                Ok(chain_config.token_freeze_fee(block_height))
            }
            AccountCommand::ChangeTokenAuthority(_, _) => {
                Ok(chain_config.token_change_authority_fee(block_height))
            }
        }
    }

    pub fn from_outputs(
        chain_config: &ChainConfig,
        block_height: BlockHeight,
        outputs: &[TxOutput],
    ) -> Result<Self, Error> {
        let mut accumulator = Self::new();

        for output in outputs {
            match output {
                TxOutput::Transfer(value, _) | TxOutput::Burn(value) => match value {
                    OutputValue::Coin(amount) => insert_or_increase(
                        &mut accumulator.unconstrained_value,
                        CoinOrTokenId::Coin,
                        *amount,
                    )?,
                    OutputValue::TokenV0(_) => { /* ignore */ }
                    OutputValue::TokenV1(token_id, amount) => insert_or_increase(
                        &mut accumulator.unconstrained_value,
                        CoinOrTokenId::TokenId(*token_id),
                        *amount,
                    )?,
                },
                TxOutput::DelegateStaking(coins, _) => insert_or_increase(
                    &mut accumulator.unconstrained_value,
                    CoinOrTokenId::Coin,
                    *coins,
                )?,
                TxOutput::CreateStakePool(_, data) => insert_or_increase(
                    &mut accumulator.unconstrained_value,
                    CoinOrTokenId::Coin,
                    data.pledge(),
                )?,
                TxOutput::ProduceBlockFromStake(_, _) | TxOutput::CreateDelegationId(_, _) => {
                    /* do nothing as these outputs cannot produce values */
                }
                TxOutput::LockThenTransfer(value, _, timelock) => match value {
                    OutputValue::Coin(coins) => {
                        accumulator.process_output_timelock(timelock, *coins)?;
                    }
                    OutputValue::TokenV0(_) => { /* ignore */ }
                    OutputValue::TokenV1(token_id, amount) => insert_or_increase(
                        &mut accumulator.unconstrained_value,
                        CoinOrTokenId::TokenId(*token_id),
                        *amount,
                    )?,
                },
                TxOutput::DataDeposit(_) => insert_or_increase(
                    &mut accumulator.unconstrained_value,
                    CoinOrTokenId::Coin,
                    chain_config.data_deposit_fee(),
                )?,
                TxOutput::IssueFungibleToken(_) => insert_or_increase(
                    &mut accumulator.unconstrained_value,
                    CoinOrTokenId::Coin,
                    chain_config.fungible_token_issuance_fee(),
                )?,
                TxOutput::IssueNft(_, _, _) => insert_or_increase(
                    &mut accumulator.unconstrained_value,
                    CoinOrTokenId::Coin,
                    chain_config.nft_issuance_fee(block_height),
                )?,
            };
        }

        Ok(accumulator)
    }

    fn process_output_timelock(
        &mut self,
        timelock: &OutputTimeLock,
        locked_coins: Amount,
    ) -> Result<(), Error> {
        match timelock {
            OutputTimeLock::UntilHeight(_)
            | OutputTimeLock::UntilTime(_)
            | OutputTimeLock::ForSeconds(_) => insert_or_increase(
                &mut self.unconstrained_value,
                CoinOrTokenId::Coin,
                locked_coins,
            )?,
            OutputTimeLock::ForBlockCount(block_count) => {
                match NonZeroU64::new(*block_count) {
                    Some(block_count) => insert_or_increase(
                        &mut self.timelock_constrained,
                        block_count,
                        locked_coins,
                    )?,
                    None => insert_or_increase(
                        &mut self.unconstrained_value,
                        CoinOrTokenId::Coin,
                        locked_coins,
                    )?,
                };
            }
        };

        Ok(())
    }

    // Satisfy current constraints with other accumulator.
    pub fn satisfy_with(mut self, other: Self) -> Result<AccumulatedFee, Error> {
        for (key, value) in other.unconstrained_value {
            decrease_or(
                &mut self.unconstrained_value,
                key,
                value,
                Error::AttemptToPrintMoneyOrViolateTimelockConstraints(key),
            )?;
        }

        for (timelock, locked_coins) in other.timelock_constrained {
            // if the output cannot satisfy any constraints then use it falls back to unconstrained
            let mut constraint_range_iter = self
                .unconstrained_value
                .get_mut(&CoinOrTokenId::Coin)
                .into_iter()
                .chain(
                    self.timelock_constrained
                        .range_mut((
                            std::ops::Bound::Unbounded,
                            std::ops::Bound::Included(timelock),
                        ))
                        .map(|(_, v)| v),
                )
                .rev()
                .peekable();

            // iterate over the range until current output coins are completely used
            // or all suitable constraints are satisfied
            let mut output_coins = locked_coins;
            while output_coins > Amount::ZERO {
                let constrained_coins = constraint_range_iter.peek_mut().ok_or(
                    Error::AttemptToPrintMoneyOrViolateTimelockConstraints(CoinOrTokenId::Coin),
                )?;

                if output_coins > **constrained_coins {
                    // satisfy current constraint completely and move on to the next one
                    output_coins = (output_coins - **constrained_coins).expect("cannot fail");
                    **constrained_coins = Amount::ZERO;
                    constraint_range_iter.next();
                } else {
                    // satisfy current constraint partially and exit the loop
                    **constrained_coins =
                        (**constrained_coins - output_coins).expect("cannot fail");
                    output_coins = Amount::ZERO;
                }
            }
        }

        Ok(AccumulatedFee::from_data(
            self.unconstrained_value,
            self.timelock_constrained,
        ))
    }
}

fn decrease_or(
    total_amounts: &mut BTreeMap<CoinOrTokenId, Amount>,
    key: CoinOrTokenId,
    amount: Amount,
    error: Error,
) -> Result<(), Error> {
    if amount > Amount::ZERO {
        match total_amounts.get_mut(&key) {
            Some(value) => {
                *value = (*value - amount).ok_or(error)?;
            }
            None => {
                return Err(error);
            }
        }
    }
    Ok(())
}
