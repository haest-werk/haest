use super::*;

#[test]
fn a_spec_splits_on_the_first_colon_only() {
    assert_eq!(
        parse_model_spec("openrouter:x/y:free").unwrap(),
        ("openrouter", "x/y:free")
    );
    for bad in ["no-provider", ":model"] {
        assert!(matches!(
            parse_model_spec(bad),
            Err(ModelError::BadSpec { spec }) if spec == bad
        ));
    }
}
