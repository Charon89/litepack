//! The worker profile's removal (`lpk_core::foreign::remove_worker_profile`, the GUI's About box,
//! `docs/notes/e7-design.md` section 5) on a `LitePack.Test.<pid>.<n>` profile of its own, never
//! the real `LitePack.Worker`: a profile that exists is removed, and removing it again says there
//! was none.
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use lpk_core::foreign::{remove_profile_named, ProfileRemoval};

/// Deletes the profile at the end, also on a panic.
struct Profile(String);

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = lpk_sandbox_sys::delete_appcontainer_profile(&self.0);
    }
}

#[test]
fn an_existing_profile_is_removed_and_a_missing_one_is_reported() {
    let p = Profile(format!("LitePack.Test.{}.remove", std::process::id()));
    assert!(lpk_sandbox_sys::create_appcontainer_profile(&p.0).unwrap());
    assert_eq!(remove_profile_named(&p.0).unwrap(), ProfileRemoval::Removed);
    // Gone: a second create makes it anew.
    assert_eq!(
        remove_profile_named(&p.0).unwrap(),
        ProfileRemoval::NoneExisted
    );
    assert!(lpk_sandbox_sys::create_appcontainer_profile(&p.0).unwrap());
}

#[test]
fn a_name_that_never_had_a_profile_is_reported_as_none() {
    let name = format!("LitePack.Test.{}.never", std::process::id());
    assert_eq!(
        remove_profile_named(&name).unwrap(),
        ProfileRemoval::NoneExisted
    );
}
