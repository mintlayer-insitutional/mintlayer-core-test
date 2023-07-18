// Copyright (c) 2022 RBB S.r.l
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

use common::chain::ChainConfig;
use tx_verifier::error::TokenIssuanceError;
use utils::ensure;

fn check_is_text_alphanumeric(str: &[u8]) -> bool {
    match String::from_utf8(str.to_vec()) {
        Ok(text) => text.chars().all(char::is_alphanumeric),
        Err(_) => false,
    }
}

pub fn is_rfc3986_valid_symbol(ch: char) -> bool {
    // RFC 3986 alphabet taken from https://www.rfc-editor.org/rfc/rfc3986#section-2.1
    "%:/?#[]@!$&\'()*+,;=-._~".chars().any(|rfc1738_ch| ch == rfc1738_ch)
}

pub fn is_uri_valid(uri: &[u8]) -> bool {
    match String::from_utf8(uri.to_vec()) {
        Ok(uri) => uri
            .chars()
            // TODO: this is probably an invalid way to validate URLs. Find the proper way to do this in rust.
            .all(|ch| ch.is_alphanumeric() || is_rfc3986_valid_symbol(ch)),
        Err(_) => false,
    }
}

pub fn check_media_hash(chain_config: &ChainConfig, hash: &[u8]) -> Result<(), TokenIssuanceError> {
    ensure!(
        hash.len() >= chain_config.min_hash_len(),
        TokenIssuanceError::MediaHashTooShort
    );
    ensure!(
        hash.len() <= chain_config.max_hash_len(),
        TokenIssuanceError::MediaHashTooLong
    );
    Ok(())
}

pub fn check_token_ticker(
    chain_config: &ChainConfig,
    ticker: &[u8],
) -> Result<(), TokenIssuanceError> {
    // Check length
    ensure!(
        ticker.len() <= chain_config.token_max_ticker_len() && !ticker.is_empty(),
        TokenIssuanceError::IssueErrorInvalidTickerLength
    );

    // Check is ticker has alphanumeric chars
    ensure!(
        check_is_text_alphanumeric(ticker),
        TokenIssuanceError::IssueErrorTickerHasNoneAlphaNumericChar
    );
    Ok(())
}

pub fn check_nft_name(chain_config: &ChainConfig, name: &[u8]) -> Result<(), TokenIssuanceError> {
    // Check length
    ensure!(
        name.len() <= chain_config.token_max_name_len() && !name.is_empty(),
        TokenIssuanceError::IssueErrorInvalidNameLength
    );

    // Check is name has alphanumeric chars
    ensure!(
        check_is_text_alphanumeric(name),
        TokenIssuanceError::IssueErrorNameHasNoneAlphaNumericChar
    );
    Ok(())
}

pub fn check_nft_description(
    chain_config: &ChainConfig,
    description: &[u8],
) -> Result<(), TokenIssuanceError> {
    // Check length
    ensure!(
        description.len() <= chain_config.token_max_description_len() && !description.is_empty(),
        TokenIssuanceError::IssueErrorInvalidDescriptionLength
    );

    // Check is description has alphanumeric chars
    ensure!(
        check_is_text_alphanumeric(description),
        TokenIssuanceError::IssueErrorDescriptionHasNoneAlphaNumericChar
    );
    Ok(())
}
