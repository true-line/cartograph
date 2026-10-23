use super::*;

fn activation() -> McpTaskActivation {
    McpTaskActivation::granted(
        McpClientId::from_host_bytes([7; 16]),
        NonZeroUsize::new(3).unwrap(),
    )
}

// AC-0320/0321: runtime primitive only; no credential or transport acceptance.
#[test]
fn mcp_grant_default_revoke_drop_and_observation() {
    assert_eq!(
        McpTaskActivation::default().session().with_grant(|_, _| ()),
        Err(McpGrantUnavailable)
    );
    let owner = activation();
    let session = owner.session();
    let clone = session.clone();
    let snapshot = owner.owner_snapshot().unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap(),
        serde_json::json!({
            "client_id": snapshot.client_id, "maximum_identities": 3
        })
    );
    assert_eq!(
        session.with_grant(|id, cap| (id.clone(), cap)).unwrap(),
        (snapshot.client_id.clone(), snapshot.maximum_identities)
    );
    owner.revoke();
    assert_eq!(owner.owner_snapshot().unwrap(), None);
    assert_eq!(owner.selected_client().unwrap(), Some(snapshot.client_id));
    assert_eq!(
        session.with_grant(|_, _| panic!("revoked operation ran")),
        Err::<(), _>(McpGrantUnavailable)
    );
    assert_eq!(clone.with_grant(|_, _| ()), Err(McpGrantUnavailable));
    let owner = activation();
    let session = owner.session();
    drop(owner);
    assert_eq!(session.with_grant(|_, _| ()), Err(McpGrantUnavailable));
}

#[test]
fn mcp_grant_successor_and_renewal_never_revive_old_sessions() {
    let owner = activation();
    let old = owner.session();
    let next = owner.read_only_successor().unwrap();
    assert_eq!(
        next.selected_client().unwrap(),
        owner.selected_client().unwrap()
    );
    assert_eq!(next.owner_snapshot().unwrap(), None);
    assert_eq!(old.with_grant(|_, _| ()), Err(McpGrantUnavailable));
    assert_eq!(
        next.session().with_grant(|_, _| ()),
        Err(McpGrantUnavailable)
    );
    let renewed = McpTaskActivation::granted(
        next.selected_client().unwrap().unwrap(),
        NonZeroUsize::new(1).unwrap(),
    );
    assert_eq!(renewed.session().with_grant(|_, cap| cap.get()), Ok(1));
    assert_eq!(old.with_grant(|_, _| ()), Err(McpGrantUnavailable));
    assert_eq!(
        next.session().with_grant(|_, _| ()),
        Err(McpGrantUnavailable)
    );
}

#[test]
fn mcp_grant_poison_is_unavailable() {
    let owner = activation();
    let session = owner.session();
    let _ = std::panic::catch_unwind(|| {
        let _guard = owner.0.identity.write().unwrap();
        panic!("synthetic poison");
    });
    assert_eq!(owner.owner_snapshot(), Err(McpGrantUnavailable));
    assert_eq!(owner.selected_client(), Err(McpGrantUnavailable));
    assert!(owner.read_only_successor().is_err());
    assert_eq!(session.with_grant(|_, _| ()), Err(McpGrantUnavailable));
}

#[test]
fn mcp_grant_revocation_orders_after_admitted_operation() {
    let owner = activation();
    let session = owner.session();
    let (entered, entry) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let operation_session = session.clone();
        let operation = scope.spawn(move || {
            operation_session.with_grant(|_, _| {
                entered.send(()).unwrap();
                released.recv().unwrap();
                42
            })
        });
        entry.recv().unwrap();
        // The actual lock is held throughout admission, independent of scheduling.
        assert!(matches!(
            owner.0.identity.try_write(),
            Err(std::sync::TryLockError::WouldBlock)
        ));
        let revocation = scope.spawn(|| owner.revoke());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !owner.0.revoked.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let revoking = owner.0.revoked.load(Ordering::SeqCst);
        // New admissions refuse while the original reader still holds the lock.
        let denied = revoking.then(|| session.with_grant(|_, _| ()));
        release.send(()).unwrap();
        assert_eq!(operation.join().unwrap(), Ok(42));
        revocation.join().unwrap();
        assert_eq!(denied, Some(Err(McpGrantUnavailable)));
    });
    assert_eq!(session.with_grant(|_, _| ()), Err(McpGrantUnavailable));
}
