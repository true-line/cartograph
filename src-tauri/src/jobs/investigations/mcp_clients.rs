//! Durable identities only. No credential, activation, grant, or provider consent
//! is persisted here. Human IPC owns creation and selection of these identities.
use super::*;
use std::num::NonZeroUsize;

impl JobStore {
    pub fn create_mcp_client(
        &mut self,
        maximum_clients: NonZeroUsize,
    ) -> Result<McpClientId, InvestigationStoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let count: i64 =
            tx.query_row("SELECT count(*) FROM investigation_mcp_clients", [], |r| {
                r.get(0)
            })?;
        if usize::try_from(count).map_err(|_| InvestigationStoreError::Invalid)?
            >= maximum_clients.get()
        {
            return Err(InvestigationStoreError::Capacity);
        }
        // The identifier is opaque rather than secret. Its bytes are assigned by
        // the host, never accepted as an identity-creation argument from clients.
        let bytes: Vec<u8> = tx.query_row("SELECT randomblob(16)", [], |r| r.get(0))?;
        let client = McpClientId::from_host_bytes(
            bytes
                .try_into()
                .map_err(|_| InvestigationStoreError::Invalid)?,
        );
        one(tx.execute(
            "INSERT INTO investigation_mcp_clients(id,created_at) VALUES (?1,?2)",
            params![client.as_str(), timestamp(&tx)?],
        )?)?;
        tx.commit()?;
        Ok(client)
    }

    /// Validate a host selection without loading registry pages or creating it.
    pub fn mcp_client_exists(&self, client: &McpClientId) -> Result<bool, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let exists = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM investigation_mcp_clients WHERE id=?1)",
            [client.as_str()],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(exists)
    }

    /// Owner-only keyset page. Creation capacity does not limit reads of already
    /// admitted principals. No grant or credential is stored or returned.
    pub fn mcp_clients(
        &self,
        after: Option<&McpClientId>,
        limit: NonZeroUsize,
    ) -> Result<Vec<McpClientId>, InvestigationStoreError> {
        let limit = i64::try_from(limit.get()).map_err(|_| InvestigationStoreError::Capacity)?;
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        // CASE bounds the SQLite-to-Rust copy even for malformed stored rows.
        // Do not filter invalid IDs out and silently present an incomplete page.
        let clients = {
            let mut statement = tx.prepare(
                "SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))=32 THEN id END FROM investigation_mcp_clients WHERE (?1 IS NULL OR id>?1) ORDER BY id LIMIT ?2",
            )?;
            let mut rows = statement.query(params![after.map(McpClientId::as_str), limit])?;
            let mut clients = Vec::new();
            while let Some(row) = rows.next()? {
                let text: Option<String> = row.get(0)?;
                let text = text.ok_or(InvestigationStoreError::Invalid)?;
                let client = McpClientId::deserialize(serde::de::value::StrDeserializer::<
                    serde::de::value::Error,
                >::new(&text))
                .map_err(|_| InvestigationStoreError::Invalid)?;
                clients.push(client);
            }
            clients
        };
        tx.commit()?;
        Ok(clients)
    }
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use super::*;

    #[test]
    fn mcp_client_pages_survive_restart_and_full_creation_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = JobStore::open(&path).unwrap();
        let cap = NonZeroUsize::new(3).unwrap();
        let mut expected = (0..3)
            .map(|_| store.create_mcp_client(cap).unwrap())
            .collect::<Vec<_>>();
        expected.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        assert_eq!(
            store.create_mcp_client(cap).unwrap_err(),
            InvestigationStoreError::Capacity
        );
        drop(store);
        let store = JobStore::open(&path).unwrap();
        let one = NonZeroUsize::new(1).unwrap();
        assert_eq!(store.mcp_clients(None, one).unwrap(), expected[..1]);
        assert_eq!(
            store.mcp_clients(Some(&expected[0]), one).unwrap(),
            expected[1..2]
        );
        assert_eq!(
            store.mcp_clients(Some(&expected[1]), cap).unwrap(),
            expected[2..]
        );
        assert!(
            store
                .mcp_clients(Some(&expected[2]), cap)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.mcp_clients(None, cap).unwrap(), expected);
        // Stored malformed text must not be hidden from an owner page.
        store
            .conn
            .execute(
                "UPDATE investigation_mcp_clients SET id=?1 WHERE id=?2",
                params!["Z".repeat(32), expected[0].as_str()],
            )
            .unwrap();
        assert_eq!(
            store.mcp_clients(None, cap).unwrap_err(),
            InvestigationStoreError::Invalid
        );
        // SQLite length counts characters; acquisition must count UTF-8 bytes.
        store
            .conn
            .execute(
                "UPDATE investigation_mcp_clients SET id=?1 WHERE id=?2",
                params!["é".repeat(32), "Z".repeat(32)],
            )
            .unwrap();
        assert_eq!(
            store.mcp_clients(None, cap).unwrap_err(),
            InvestigationStoreError::Invalid
        );
    }

    // AC-0321/0327: restart restores only identity, bounded creation never evicts
    // old identities, and pages expose identities without restoring task authority.
    #[test]
    fn mcp_client_registry_is_durable_bounded_and_contains_no_grants() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = JobStore::open(&path).unwrap();
        let one = NonZeroUsize::new(1).unwrap();
        let first = store.create_mcp_client(one).unwrap();
        assert_eq!(
            store.mcp_clients(None, one).unwrap().as_slice(),
            std::slice::from_ref(&first)
        );
        assert_eq!(
            store.create_mcp_client(one).unwrap_err(),
            InvestigationStoreError::Capacity
        );
        drop(store);
        let store = JobStore::open(&path).unwrap();
        assert_eq!(
            store.mcp_clients(None, one).unwrap().as_slice(),
            std::slice::from_ref(&first)
        );
        let columns = store
            .conn
            .prepare("PRAGMA table_info(investigation_mcp_clients)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(columns, ["id", "created_at"]);
    }
}
