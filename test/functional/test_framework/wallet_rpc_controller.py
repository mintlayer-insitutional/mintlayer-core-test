#!/usr/bin/env python3
#  Copyright (c) 2022-2023 RBB S.r.l
#  Copyright (c) 2014-2021 The Bitcoin Core developers
#  opensource@mintlayer.org
#  SPDX-License-Identifier: MIT
#  Licensed under the MIT License;
#  you may not use this file except in compliance with the License.
#  You may obtain a copy of the License at
#
#  https://github.com/mintlayer/mintlayer-core/blob/master/LICENSE
#
#  Unless required by applicable law or agreed to in writing, software
#  distributed under the License is distributed on an "AS IS" BASIS,
#  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
#  See the License for the specific language governing permissions and
#  limitations under the License.
"""A wrapper around a RPC wallet instance"""

import os
import asyncio
import http.client
import json
import re
from dataclasses import dataclass
from tempfile import NamedTemporaryFile

from typing import Optional, List, Tuple, Union

ONE_MB = 2**20
READ_TIMEOUT_SEC = 30
DEFAULT_ACCOUNT_INDEX = 0

@dataclass
class UtxoOutpoint:
    id: str
    index: int

    def __str__(self):
        return f'tx({self.id},{self.index})'

@dataclass
class PoolData:
    pool_id: str
    balance: str

@dataclass
class DelegationData:
    delegation_id: str
    balance: str

@dataclass
class CreatedBlockInfo:
    block_id: str
    block_height: str

class WalletRpcController:

    def __init__(self, node, config, log, wallet_args: List[str] = [], chain_config_args: List[str] = []):
        self.log = log
        self.node = node
        self.config = config
        self.wallet_args = wallet_args
        self.chain_config_args = chain_config_args
        self.account = {'account': DEFAULT_ACCOUNT_INDEX}

    async def __aenter__(self):
        cookie_file = os.path.join(self.node.datadir, ".cookie")

        self.log.info(f"node url: {self.node.url}")
        wallet_rpc = os.path.join(self.config["environment"]["BUILDDIR"], "test_rpc_wallet"+self.config["environment"]["EXEEXT"] )
        wallet_args = ["--chain-type", "regtest", "--node-rpc-address", self.node.url.split("@")[1], "--node-cookie-file", cookie_file, "--rpc-no-authentication"] + self.wallet_args + self.chain_config_args
        self.wallet_log_file = NamedTemporaryFile(prefix="wallet_stderr_", dir=os.path.dirname(self.node.datadir), delete=False)
        self.wallet_commands_file = NamedTemporaryFile(prefix="wallet_commands_responses_", dir=os.path.dirname(self.node.datadir), delete=False)
        url = "127.0.0.1"
        port = 23034

        self.process = await asyncio.create_subprocess_exec(
            wallet_rpc, *wallet_args,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=self.wallet_log_file,
        )
        self.http_client = http.client.HTTPConnection(url, port)
        await asyncio.sleep(5)
        return self

    async def __aexit__(self, exc_type, exc_value, traceback):
        self.log.debug("exiting wallet")
        self._write_command("shutdown")
        await self.process.communicate()
        self.http_client.close()
        self.wallet_log_file.close()
        self.wallet_commands_file.close()

    def _write_command(self, method: str, params = []) -> dict:
        encoded_cmd = method.encode()
        encoded_params = str(params).encode()
        self.wallet_commands_file.write(b"writhing command: ")
        self.wallet_commands_file.write(encoded_cmd)
        self.wallet_commands_file.write(encoded_params)

        headers = {"Content-Type": "application/json"}
        payload = {
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1  # Adjust the id field as needed
        }
        encoded_payload = json.dumps(payload).encode('utf-8')
        self.http_client.request("POST", '', body=encoded_payload, headers=headers)
        response = self.http_client.getresponse()
        self.log.info(f"method, {method}")
        self.log.info(f'response, {response} status: {response.status}')
        body = response.read().decode('utf-8')
        self.log.info(f'body, {body}')
        self.wallet_commands_file.write(response.read())
        return json.loads(body)

    async def create_wallet(self, name: str = "wallet") -> str:
        wallet_file = os.path.join(self.node.datadir, name)
        self._write_command("wallet_create", [wallet_file, True])
        return "Success"

    async def open_wallet(self, name: str = "wallet") -> str:
        wallet_file = os.path.join(self.node.datadir, name)
        self._write_command("wallet_open", [wallet_file, None])
        return "Success"

    async def close_wallet(self) -> str:
        return self._write_command("wallet-close", [])

    async def get_best_block_height(self) -> str:
        return str(self._write_command("wallet_best_block", [{}])['result']['height'])

    async def get_best_block(self) -> str:
        return self._write_command("wallet_best_block", [{}])['result']['id']

    async def create_new_account(self, name: Optional[str] = None) -> str:
        self._write_command("account_create", [name, {}])
        return "Success"

    async def select_account(self, account_index: int) -> str:
        self.account = {'account': account_index}
        return "Success"

    async def new_public_key(self) -> bytes:
        addr = await self.new_address()
        public_key = self._write_command("address_reveal_public_key", [self.account, addr])['result']['public_key_hex']

        # remove the pub key enum value, the first one byte
        pub_key_bytes = bytes.fromhex(public_key)[1:]
        return pub_key_bytes

    async def new_address(self) -> str:
        return self._write_command(f"address_new", [self.account])['result']['address']

    async def list_utxos(self, utxo_types: str = '', with_locked: str = '', utxo_states: List[str] = []) -> List[UtxoOutpoint]:
        outputs = self._write_command("account_utxos", [self.account, utxo_types, with_locked, ''.join(utxo_states)])['result']
        return [UtxoOutpoint(id=match['outpoint']['id'].strip(), index=int(match['outpoint']['index'].strip())) for match in outputs]

    async def get_transaction(self, tx_id: str) -> str:
        return self._write_command("transaction_get", [self.account, tx_id])['result']

    async def get_raw_signed_transaction(self, tx_id: str) -> str:
        return self._write_command("transaction_get_signed_raw", [self.account, tx_id])['result']

    async def send_to_address(self, address: str, amount: int, selected_utxos: List[UtxoOutpoint] = []) -> str:
        self._write_command("address_send", [self.account, address, str(amount), selected_utxos, {'in_top_x_mb': 5}])
        return "The transaction was submitted successfully"

    async def send_tokens_to_address(self, token_id: str, address: str, amount: Union[float, str]):
        return self._write_command("token_send", [self.account, token_id, address, amount, {'in_top_x_mb': 5}])['result']

    async def issue_new_token(self,
                              token_ticker: str,
                              number_of_decimals: int,
                              metadata_uri: str,
                              destination_address: str,
                              token_supply: str = 'unlimited',
                              is_freezable: str = 'freezable'):
        output = self._write_command('token_issue_new', [
            self.account,
            token_ticker,
            number_of_decimals,
            metadata_uri,
            destination_address,
            token_supply,
            is_freezable,
            {'in_top_x_mb': 5}
            ])['result']
        return output

    async def mint_tokens(self, token_id: str, address: str, amount: int) -> str:
        return self._write_command("token_mint", [self.account, token_id, address, amount, {'in_top_x_mb': 5}])['result']

    async def unmint_tokens(self, token_id: str, amount: int) -> str:
        return self._write_command("token_mint", [self.account, token_id, amount, {'in_top_x_mb': 5}])['result']

    async def lock_token_supply(self, token_id: str) -> str:
        return self._write_command("token_lock_supply", [self.account, token_id, {'in_top_x_mb': 5}])['result']

    async def freeze_token(self, token_id: str, is_unfreezable: str) -> str:
        return self._write_command("token_freeze", [self.account, token_id, is_unfreezable, {'in_top_x_mb': 5}])['result']

    async def unfreeze_token(self, token_id: str) -> str:
        return self._write_command("token_unfreeze", [self.account, token_id, {'in_top_x_mb': 5}])['result']

    async def change_token_authority(self, token_id: str, new_authority: str) -> str:
        return self._write_command("token_change_authority", [self.account, token_id, new_authority, {'in_top_x_mb': 5}])['result']

    async def issue_new_nft(self,
                            destination_address: str,
                            media_hash: str,
                            name: str,
                            description: str,
                            ticker: str,
                            creator: Optional[str] = '',
                            icon_uri: Optional[str] = '',
                            media_uri: Optional[str] = '',
                            additional_metadata_uri: Optional[str] = ''):
        output = self._write_command("token_nft_issue_new", [
            self.account,
            destination_address,
            {
                'media_hash': media_hash,
                'name': name,
                'description': description,
                'ticker': ticker,
                'creator': creator,
                'icon_uri': icon_uri,
                'media_uri': media_uri,
                'additional_metadata_uri': additional_metadata_uri
            },
            {'in_top_x_mb': 5}
            ])['result']
        return output

    async def create_stake_pool(self,
                                amount: int,
                                cost_per_block: int,
                                margin_ratio_per_thousand: float,
                                decommission_key: Optional[str] = None) -> str:
        #decommission_key = decommission_key if decommission_key else 'NULL'
        self._write_command("staking_create_pool", [self.account, str(amount), str(cost_per_block), str(margin_ratio_per_thousand), decommission_key, {'in_top_x_mb': 5}])['result']
        return "The transaction was submitted successfully"

    async def decommission_stake_pool(self, pool_id: str, address: str) -> str:
        self._write_command("staking_decommission_pool", [self.account, pool_id, address, {'in_top_x_mb': 5}])['result']
        return "The transaction was submitted successfully"

    async def list_pool_ids(self) -> List[PoolData]:
        pools = self._write_command("staking_list_pool_ids", [self.account])['result']
        return [PoolData(pool['pool_id'], pool['balance']) for pool in pools]

    async def list_created_blocks_ids(self) -> List[CreatedBlockInfo]:
        output = self._write_command("staking_list_created_block_ids", [self.account])['result']
        return [CreatedBlockInfo(block['id'], block['height']) for block in output]

    async def create_delegation(self, address: str, pool_id: str) -> Optional[str]:
        return self._write_command("delegation_create", [self.account, address, pool_id, {'in_top_x_mb': 5}])['result']['delegation_id']

    async def stake_delegation(self, amount: int, delegation_id: str) -> str:
        self._write_command(f"delegation_stake", [self.account, str(amount), delegation_id, {'in_top_x_mb': 5}])['result']
        return "Success"

    async def list_delegation_ids(self) -> List[DelegationData]:
        delegations = self._write_command("delegation_list_ids", [self.account])['result']
        return [DelegationData(delegation['delegation_id'], delegation['balance']) for delegation in delegations]

    async def deposit_data(self, data: str) -> str:
        return self._write_command("address_deposit_data", [self.account, data, {'in_top_x_mb': 5}])['result']

    async def sync(self) -> str:
        self._write_command("wallet_sync")
        return "Success"

    async def start_staking(self) -> str:
        self._write_command(f"staking_start", [self.account])['result']
        return "Staking started successfully"

    async def stop_staking(self) -> str:
        self._write_command(f"staking_stop", [self.account])['result']
        return "Success"

    async def staking_status(self) -> str:
        result = self._write_command(f"staking_status", [self.account])['result']
        if result == "Staking":
            return "Staking"
        else:
            return "Not staking"

    async def get_addresses_usage(self) -> str:
        return self._write_command("address_show")['result']

    async def get_balance(self, with_locked: str = 'unlocked', utxo_states: List[str] = ['confirmed']) -> str:
        with_locked = with_locked.capitalize()
        balances = self._write_command("account_balance", [self.account, with_locked])# {' '.join(utxo_states)})
        return f"Coins amount: {balances['result']['coins']}"

    async def list_pending_transactions(self) -> List[str]:
        output = self._write_command("transaction_list_pending", [self.account])['result']
        return output

    async def abandon_transaction(self, tx_id: str) -> str:
        return self._write_command("transaction_abandon", [self.account, tx_id])['result']

