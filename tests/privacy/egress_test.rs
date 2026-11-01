#![allow(dead_code)]

#[path = "../../src/context/model.rs"]
mod model;
#[path = "../../src/context/source.rs"]
mod source;

use model::{EgressPolicy, LookupStatus};

#[test]
fn egress_denial_distinct() {
    let p = EgressPolicy::deny("no egress");
    assert!(p.enforce().is_err());
    let a = EgressPolicy::allow();
    assert!(a.enforce().is_ok());
}

#[test]
fn cross_scope_default_same_namespace() {
    assert!(source::same_namespace("ns", "ns"));
    assert!(source::cross_scope_authorized("ns", "ns", &[]));
    assert!(!source::cross_scope_authorized("ns", "other", &[]));
    assert!(source::cross_scope_authorized("ns", "other", &["other".to_string()]));
}

#[test]
fn lookup_status_variants() {
    assert_ne!(LookupStatus::Denied, LookupStatus::NotFound);
}
