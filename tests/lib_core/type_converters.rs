use std::str::FromStr;

use rgb_lightning_node::{
    Bolt11Invoice, ChannelId, ContractId, PaymentHash, PublicKey, RecipientId, TransportEndpoint,
    Txid, UniffiCustomTypeConverter,
};

#[test]
fn custom_types_roundtrip() {
    let public_key = PublicKey::from_str(
        "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
    )
    .unwrap();
    let built_in = <PublicKey as UniffiCustomTypeConverter>::from_custom(public_key);
    let roundtrip = <PublicKey as UniffiCustomTypeConverter>::into_custom(built_in).unwrap();
    assert_eq!(roundtrip, public_key);

    let txid =
        Txid::from_str("4d3f1f0f87f63a01d4fce1ab4cf8fe0cf5e8f7ff7f6ba6748b6ff1571318dd43").unwrap();
    let built_in = <Txid as UniffiCustomTypeConverter>::from_custom(txid);
    let roundtrip = <Txid as UniffiCustomTypeConverter>::into_custom(built_in).unwrap();
    assert_eq!(roundtrip, txid);

    let channel_id = lightning::ln::types::ChannelId([1u8; 32]);
    let built_in = <ChannelId as UniffiCustomTypeConverter>::from_custom(channel_id);
    let roundtrip = <ChannelId as UniffiCustomTypeConverter>::into_custom(built_in).unwrap();
    assert_eq!(roundtrip.0, [1u8; 32]);

    let payment_hash = lightning::types::payment::PaymentHash([2u8; 32]);
    let built_in = <PaymentHash as UniffiCustomTypeConverter>::from_custom(payment_hash);
    let roundtrip = <PaymentHash as UniffiCustomTypeConverter>::into_custom(built_in).unwrap();
    assert_eq!(roundtrip.0, [2u8; 32]);
}

#[test]
fn custom_types_reject_invalid_values() {
    assert!(<ChannelId as UniffiCustomTypeConverter>::into_custom("deadbeef".to_string()).is_err());
    assert!(<PaymentHash as UniffiCustomTypeConverter>::into_custom("deadbeef".to_string()).is_err());
    assert!(<ContractId as UniffiCustomTypeConverter>::into_custom("not-a-contract-id".to_string()).is_err());
    assert!(<Bolt11Invoice as UniffiCustomTypeConverter>::into_custom("not-an-invoice".to_string()).is_err());
    assert!(<RecipientId as UniffiCustomTypeConverter>::into_custom("not-recipient-id".to_string()).is_err());
    assert!(<TransportEndpoint as UniffiCustomTypeConverter>::into_custom("not-a-transport-endpoint".to_string()).is_err());
}
