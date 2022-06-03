use super::utils::*;
use super::{super::sighashtype::SigHashType, TestData};
use crate::{
    address::pubkeyhash::PublicKeyHash,
    chain::{signature::TransactionSigError, Destination},
    primitives::{Id, H256},
};
use crypto::key::{KeyKind, PrivateKey};
use script::Script;

#[test]
fn sign_and_verify_sighash_all() {
    let (private_key, public_key) = PrivateKey::new(KeyKind::RistrettoSchnorr);
    // Sign all inputs
    let test_data: TestData = vec![
        // SigHashType::ALL. Destination = PubKey
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::ALL).unwrap(),
            0u32,
            31u32,
            Ok(()),
        ),
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::ALL).unwrap(),
            31u32,
            0u32,
            Ok(()),
        ),
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::ALL).unwrap(),
            20u32,
            3u32,
            Ok(()),
        ),
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::ALL).unwrap(),
            3u32,
            20u32,
            Ok(()),
        ),
        // SigHashType::ALL | SigHashType::ANYONECANPAY. Destination = PubKey
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::ALL | SigHashType::ANYONECANPAY).unwrap(),
            0u32,
            31u32,
            Ok(()),
        ),
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::ALL | SigHashType::ANYONECANPAY).unwrap(),
            31u32,
            0u32,
            Ok(()),
        ),
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::ALL | SigHashType::ANYONECANPAY).unwrap(),
            20u32,
            3u32,
            Ok(()),
        ),
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::ALL | SigHashType::ANYONECANPAY).unwrap(),
            3u32,
            20u32,
            Ok(()),
        ),
        // SigHashType::ALL. Destination = Address
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::ALL).unwrap(),
            0u32,
            31u32,
            Ok(()),
        ),
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::ALL).unwrap(),
            31u32,
            0u32,
            Ok(()),
        ),
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::ALL).unwrap(),
            20u32,
            3u32,
            Ok(()),
        ),
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::ALL).unwrap(),
            3u32,
            20u32,
            Ok(()),
        ),
        // SigHashType::ALL | SigHashType::ANYONECANPAY. Destination = Address
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::ALL | SigHashType::ANYONECANPAY).unwrap(),
            0u32,
            31u32,
            Ok(()),
        ),
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::ALL | SigHashType::ANYONECANPAY).unwrap(),
            31u32,
            0u32,
            Ok(()),
        ),
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::ALL | SigHashType::ANYONECANPAY).unwrap(),
            20u32,
            3u32,
            Ok(()),
        ),
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::ALL | SigHashType::ANYONECANPAY).unwrap(),
            3u32,
            20u32,
            Ok(()),
        ),
        // SigHashType::ALL. Destination = AnyoneCanSpend
        (
            Destination::AnyoneCanSpend,
            SigHashType::try_from(SigHashType::ALL).unwrap(),
            20u32,
            3u32,
            Err(TransactionSigError::AttemptedToProduceSignatureForAnyoneCanSpend),
        ),
        // SigHashType::ALL | SigHashType::ANYONECANPAY. Destination = AnyoneCanSpend
        (
            Destination::AnyoneCanSpend,
            SigHashType::try_from(SigHashType::ALL | SigHashType::ANYONECANPAY).unwrap(),
            20u32,
            3u32,
            Err(TransactionSigError::AttemptedToProduceSignatureForAnyoneCanSpend),
        ),
        // SigHashType::ALL. Destination = AnyoneCanSpend
        (
            Destination::ScriptHash(Id::<Script>::from(H256::random())),
            SigHashType::try_from(SigHashType::ALL).unwrap(),
            20u32,
            3u32,
            Err(TransactionSigError::Unsupported), // TODO:We have to add tests as soon as it will support
        ),
        // SigHashType::ALL | SigHashType::ANYONECANPAY. Destination = AnyoneCanSpend
        (
            Destination::ScriptHash(Id::<Script>::from(H256::random())),
            SigHashType::try_from(SigHashType::ALL | SigHashType::ANYONECANPAY).unwrap(),
            20u32,
            3u32,
            Err(TransactionSigError::Unsupported), // TODO:We have to add tests as soon as it will support
        ),
    ];

    test_data.iter().for_each(
        |(outpoint_dest, sighash_type, inputs_count, outputs_count, expected_result)| {
            let mut tx =
                generate_unsigned_tx(outpoint_dest.clone(), *inputs_count, *outputs_count).unwrap();
            match sign_whole_tx(&mut tx, &private_key, *sighash_type, outpoint_dest.clone()) {
                Ok(_) => assert_eq!(verify_signed_tx(&tx, outpoint_dest), *expected_result),
                Err(err) => assert_eq!(Err(err), *expected_result),
            }
        },
    );
}

#[test]
fn sign_and_verify_sighash_single() {
    let (private_key, public_key) = PrivateKey::new(KeyKind::RistrettoSchnorr);
    // Sign all inputs
    let test_data: TestData = vec![
        // SigHashType::SINGLE. Destination = PubKey
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::SINGLE).unwrap(),
            0u32,
            31u32,
            Ok(()),
        ),
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::SINGLE).unwrap(),
            31u32,
            0u32,
            Err(TransactionSigError::InvalidInputIndex(0, 0)),
        ),
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::SINGLE).unwrap(),
            21u32,
            3u32,
            Err(TransactionSigError::InvalidInputIndex(3, 3)),
        ),
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::SINGLE).unwrap(),
            3u32,
            21u32,
            Ok(()),
        ),
        // SigHashType::SINGLE | SigHashType::ANYONECANPAY. Destination = PubKey
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::SINGLE | SigHashType::ANYONECANPAY).unwrap(),
            0u32,
            33u32,
            Ok(()),
        ),
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::SINGLE | SigHashType::ANYONECANPAY).unwrap(),
            21u32,
            0u32,
            Err(TransactionSigError::InvalidInputIndex(0, 0)),
        ),
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::SINGLE | SigHashType::ANYONECANPAY).unwrap(),
            15u32,
            33u32,
            Ok(()),
        ),
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::SINGLE | SigHashType::ANYONECANPAY).unwrap(),
            21u32,
            7u32,
            Err(TransactionSigError::InvalidInputIndex(7, 7)),
        ),
        // SigHashType::SINGLE. Destination = Address
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::SINGLE).unwrap(),
            0u32,
            33u32,
            Ok(()),
        ),
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::SINGLE).unwrap(),
            21u32,
            0u32,
            Err(TransactionSigError::InvalidInputIndex(0, 0)),
        ),
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::SINGLE).unwrap(),
            15u32,
            33u32,
            Ok(()),
        ),
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::SINGLE).unwrap(),
            21u32,
            7u32,
            Err(TransactionSigError::InvalidInputIndex(7, 7)),
        ),
        // SigHashType::SINGLE | SigHashType::ANYONECANPAY. Destination = Address
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::SINGLE | SigHashType::ANYONECANPAY).unwrap(),
            0u32,
            33u32,
            Ok(()),
        ),
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::SINGLE | SigHashType::ANYONECANPAY).unwrap(),
            21u32,
            0u32,
            Err(TransactionSigError::InvalidInputIndex(0, 0)),
        ),
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::SINGLE | SigHashType::ANYONECANPAY).unwrap(),
            15u32,
            33u32,
            Ok(()),
        ),
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::SINGLE | SigHashType::ANYONECANPAY).unwrap(),
            21u32,
            7u32,
            Err(TransactionSigError::InvalidInputIndex(7, 7)),
        ),
        // SigHashType::SINGLE. Destination = AnyoneCanSpend
        (
            Destination::AnyoneCanSpend,
            SigHashType::try_from(SigHashType::SINGLE).unwrap(),
            21u32,
            33u32,
            Err(TransactionSigError::AttemptedToProduceSignatureForAnyoneCanSpend),
        ),
        // SigHashType::SINGLE | SigHashType::ANYONECANPAY. Destination = AnyoneCanSpend
        (
            Destination::AnyoneCanSpend,
            SigHashType::try_from(SigHashType::SINGLE | SigHashType::ANYONECANPAY).unwrap(),
            21u32,
            33u32,
            Err(TransactionSigError::AttemptedToProduceSignatureForAnyoneCanSpend),
        ),
        // SigHashType::SINGLE. Destination = AnyoneCanSpend
        (
            Destination::ScriptHash(Id::<Script>::from(H256::random())),
            SigHashType::try_from(SigHashType::SINGLE).unwrap(),
            21u32,
            33u32,
            Err(TransactionSigError::Unsupported), // TODO:We have to add tests as soon as it will support
        ),
        // SigHashType::SINGLE | SigHashType::ANYONECANPAY. Destination = AnyoneCanSpend
        (
            Destination::ScriptHash(Id::<Script>::from(H256::random())),
            SigHashType::try_from(SigHashType::SINGLE | SigHashType::ANYONECANPAY).unwrap(),
            21u32,
            33u32,
            Err(TransactionSigError::Unsupported), // TODO:We have to add tests as soon as it will support
        ),
    ];

    test_data.iter().for_each(
        |(outpoint_dest, sighash_type, inputs_count, outputs_count, expected_result)| {
            let mut tx =
                generate_unsigned_tx(outpoint_dest.clone(), *inputs_count, *outputs_count).unwrap();
            match sign_whole_tx(&mut tx, &private_key, *sighash_type, outpoint_dest.clone()) {
                Ok(_) => assert_eq!(verify_signed_tx(&tx, outpoint_dest), *expected_result),
                Err(err) => assert_eq!(Err(err), *expected_result),
            }
        },
    );
}

#[test]
fn sign_and_verify_sighash_none() {
    let (private_key, public_key) = PrivateKey::new(KeyKind::RistrettoSchnorr);
    // Sign all inputs
    let test_data: TestData = vec![
        // SigHashType::NONE. Destination = PubKey
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::NONE).unwrap(),
            21u32,
            33u32,
            Ok(()),
        ),
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::NONE).unwrap(),
            21u32,
            3u32,
            Ok(()),
        ),
        // SigHashType::NONE | SigHashType::ANYONECANPAY. Destination = PubKey
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::NONE | SigHashType::ANYONECANPAY).unwrap(),
            21u32,
            33u32,
            Ok(()),
        ),
        (
            Destination::PublicKey(public_key.clone()),
            SigHashType::try_from(SigHashType::NONE | SigHashType::ANYONECANPAY).unwrap(),
            21u32,
            3u32,
            Ok(()),
        ),
        // SigHashType::NONE. Destination = Address
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::NONE).unwrap(),
            21u32,
            33u32,
            Ok(()),
        ),
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::NONE).unwrap(),
            21u32,
            3u32,
            Ok(()),
        ),
        // SigHashType::NONE | SigHashType::ANYONECANPAY. Destination = Address
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::NONE | SigHashType::ANYONECANPAY).unwrap(),
            21u32,
            33u32,
            Ok(()),
        ),
        (
            Destination::Address(PublicKeyHash::from(&public_key)),
            SigHashType::try_from(SigHashType::NONE | SigHashType::ANYONECANPAY).unwrap(),
            21u32,
            3u32,
            Ok(()),
        ),
        // SigHashType::NONE. Destination = AnyoneCanSpend
        (
            Destination::AnyoneCanSpend,
            SigHashType::try_from(SigHashType::NONE).unwrap(),
            21u32,
            33u32,
            Err(TransactionSigError::AttemptedToProduceSignatureForAnyoneCanSpend),
        ),
        // SigHashType::NONE | SigHashType::ANYONECANPAY. Destination = AnyoneCanSpend
        (
            Destination::AnyoneCanSpend,
            SigHashType::try_from(SigHashType::NONE | SigHashType::ANYONECANPAY).unwrap(),
            21u32,
            33u32,
            Err(TransactionSigError::AttemptedToProduceSignatureForAnyoneCanSpend),
        ),
        // SigHashType::NONE. Destination = AnyoneCanSpend
        (
            Destination::ScriptHash(Id::<Script>::from(H256::random())),
            SigHashType::try_from(SigHashType::NONE).unwrap(),
            21u32,
            33u32,
            Err(TransactionSigError::Unsupported), // TODO:We have to add tests as soon as it will support
        ),
        // SigHashType::NONE | SigHashType::ANYONECANPAY. Destination = AnyoneCanSpend
        (
            Destination::ScriptHash(Id::<Script>::from(H256::random())),
            SigHashType::try_from(SigHashType::NONE | SigHashType::ANYONECANPAY).unwrap(),
            21u32,
            33u32,
            Err(TransactionSigError::Unsupported), // TODO:We have to add tests as soon as it will support
        ),
    ];

    test_data.iter().for_each(
        |(outpoint_dest, sighash_type, inputs_count, outputs_count, expected_result)| {
            let mut tx =
                generate_unsigned_tx(outpoint_dest.clone(), *inputs_count, *outputs_count).unwrap();
            match sign_whole_tx(&mut tx, &private_key, *sighash_type, outpoint_dest.clone()) {
                Ok(_) => assert_eq!(verify_signed_tx(&tx, outpoint_dest), *expected_result),
                Err(err) => assert_eq!(Err(err), *expected_result),
            }
        },
    );
}
