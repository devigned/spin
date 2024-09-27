use anyhow::{Context, Result};
use spin_core::{async_trait, wasmtime::component::Resource};
use spin_resource_table::Table;
use spin_world::v2::key_value;
use spin_world::wasi::keyvalue as wasi_keyvalue;
use std::{collections::HashSet, sync::Arc};
use tracing::{instrument, Level};

const DEFAULT_STORE_TABLE_CAPACITY: u32 = 256;

pub use key_value::Error;

#[async_trait]
pub trait StoreManager: Sync + Send {
    async fn get(&self, name: &str) -> Result<Arc<dyn Store>, Error>;
    fn is_defined(&self, store_name: &str) -> bool;

    /// A human-readable summary of the given store's configuration
    ///
    /// Example: "Redis at localhost:1234"
    fn summary(&self, store_name: &str) -> Option<String> {
        let _ = store_name;
        None
    }
}

#[async_trait]
pub trait Store: Sync + Send {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Error>;
    async fn set(&self, key: &str, value: &[u8]) -> Result<(), Error>;
    async fn delete(&self, key: &str) -> Result<(), Error>;
    async fn exists(&self, key: &str) -> Result<bool, Error>;
    async fn get_keys(&self) -> Result<Vec<String>, Error>;
}

pub struct KeyValueDispatch {
    allowed_stores: HashSet<String>,
    manager: Arc<dyn StoreManager>,
    stores: Table<Arc<dyn Store>>,
}

impl KeyValueDispatch {
    pub fn new(allowed_stores: HashSet<String>, manager: Arc<dyn StoreManager>) -> Self {
        Self::new_with_capacity(allowed_stores, manager, DEFAULT_STORE_TABLE_CAPACITY)
    }

    pub fn new_with_capacity(
        allowed_stores: HashSet<String>,
        manager: Arc<dyn StoreManager>,
        capacity: u32,
    ) -> Self {
        Self {
            allowed_stores,
            manager,
            stores: Table::new(capacity),
        }
    }

    pub fn get_store<T: 'static>(&self, store: Resource<T>) -> anyhow::Result<&Arc<dyn Store>> {
        self.stores.get(store.rep()).context("invalid store")
    }

    pub fn allowed_stores(&self) -> &HashSet<String> {
        &self.allowed_stores
    }

    pub fn get_store_wasi<T: 'static>(
        &self,
        store: Resource<T>,
    ) -> Result<&Arc<dyn Store>, wasi_keyvalue::store::Error> {
        self.stores
            .get(store.rep())
            .ok_or(wasi_keyvalue::store::Error::NoSuchStore)
    }
}

#[async_trait]
impl key_value::Host for KeyValueDispatch {}

#[async_trait]
impl key_value::HostStore for KeyValueDispatch {
    #[instrument(name = "spin_key_value.open", skip(self), err(level = Level::INFO), fields(otel.kind = "client", kv.backend=self.manager.summary(&name).unwrap_or("unknown".to_string())))]
    async fn open(&mut self, name: String) -> Result<Result<Resource<key_value::Store>, Error>> {
        Ok(async {
            if self.allowed_stores.contains(&name) {
                let store = self
                    .stores
                    .push(self.manager.get(&name).await?)
                    .map_err(|()| Error::StoreTableFull)?;
                Ok(Resource::new_own(store))
            } else {
                Err(Error::AccessDenied)
            }
        }
        .await)
    }

    #[instrument(name = "spin_key_value.get", skip(self, store), err(level = Level::INFO), fields(otel.kind = "client"))]
    async fn get(
        &mut self,
        store: Resource<key_value::Store>,
        key: String,
    ) -> Result<Result<Option<Vec<u8>>, Error>> {
        let store = self.get_store(store)?;
        Ok(store.get(&key).await)
    }

    #[instrument(name = "spin_key_value.set", skip(self, store, value), err(level = Level::INFO), fields(otel.kind = "client"))]
    async fn set(
        &mut self,
        store: Resource<key_value::Store>,
        key: String,
        value: Vec<u8>,
    ) -> Result<Result<(), Error>> {
        let store = self.get_store(store)?;
        Ok(store.set(&key, &value).await)
    }

    #[instrument(name = "spin_key_value.delete", skip(self, store), err(level = Level::INFO), fields(otel.kind = "client"))]
    async fn delete(
        &mut self,
        store: Resource<key_value::Store>,
        key: String,
    ) -> Result<Result<(), Error>> {
        let store = self.get_store(store)?;
        Ok(store.delete(&key).await)
    }

    #[instrument(name = "spin_key_value.exists", skip(self, store), err(level = Level::INFO), fields(otel.kind = "client"))]
    async fn exists(
        &mut self,
        store: Resource<key_value::Store>,
        key: String,
    ) -> Result<Result<bool, Error>> {
        let store = self.get_store(store)?;
        Ok(store.exists(&key).await)
    }

    #[instrument(name = "spin_key_value.get_keys", skip(self, store), err(level = Level::INFO), fields(otel.kind = "client"))]
    async fn get_keys(
        &mut self,
        store: Resource<key_value::Store>,
    ) -> Result<Result<Vec<String>, Error>> {
        let store = self.get_store(store)?;
        Ok(store.get_keys().await)
    }

    async fn drop(&mut self, store: Resource<key_value::Store>) -> Result<()> {
        self.stores.remove(store.rep());
        Ok(())
    }
}

fn to_wasi_err(e: Error) -> wasi_keyvalue::store::Error {
    match e {
        Error::AccessDenied => wasi_keyvalue::store::Error::AccessDenied,
        Error::NoSuchStore => wasi_keyvalue::store::Error::NoSuchStore,
        Error::StoreTableFull => wasi_keyvalue::store::Error::Other("store table full".to_string()),
        Error::Other(msg) => wasi_keyvalue::store::Error::Other(msg),
    }
}

#[async_trait]
impl wasi_keyvalue::store::Host for KeyValueDispatch {
    async fn open(
        &mut self,
        identifier: String,
    ) -> Result<Resource<wasi_keyvalue::store::Bucket>, wasi_keyvalue::store::Error> {
        if self.allowed_stores.contains(&identifier) {
            let store = self
                .stores
                .push(self.manager.get(&identifier).await.map_err(to_wasi_err)?)
                .map_err(|()| wasi_keyvalue::store::Error::Other("store table full".to_string()))?;
            Ok(Resource::new_own(store))
        } else {
            Err(wasi_keyvalue::store::Error::AccessDenied)
        }
    }

    fn convert_error(
        &mut self,
        error: spin_world::wasi::keyvalue::store::Error,
    ) -> std::result::Result<spin_world::wasi::keyvalue::store::Error, anyhow::Error> {
        Ok(error)
    }
}

use wasi_keyvalue::store::Bucket;
#[async_trait]
impl wasi_keyvalue::store::HostBucket for KeyValueDispatch {
    async fn get(
        &mut self,
        self_: Resource<Bucket>,
        key: String,
    ) -> Result<Option<Vec<u8>>, wasi_keyvalue::store::Error> {
        let store = self.get_store_wasi(self_)?;
        store.get(&key).await.map_err(to_wasi_err)
    }

    async fn set(
        &mut self,
        self_: Resource<Bucket>,
        key: String,
        value: Vec<u8>,
    ) -> Result<(), wasi_keyvalue::store::Error> {
        let store = self.get_store_wasi(self_)?;
        store.set(&key, &value).await.map_err(to_wasi_err)
    }

    async fn delete(
        &mut self,
        self_: Resource<Bucket>,
        key: String,
    ) -> Result<(), wasi_keyvalue::store::Error> {
        let store = self.get_store_wasi(self_)?;
        store.delete(&key).await.map_err(to_wasi_err)
    }

    async fn exists(
        &mut self,
        self_: Resource<Bucket>,
        key: String,
    ) -> Result<bool, wasi_keyvalue::store::Error> {
        let store = self.get_store_wasi(self_)?;
        store.exists(&key).await.map_err(to_wasi_err)
    }

    async fn list_keys(
        &mut self,
        self_: Resource<Bucket>,
        cursor: Option<u64>,
    ) -> Result<wasi_keyvalue::store::KeyResponse, wasi_keyvalue::store::Error> {
        if cursor.unwrap_or_default() != 0 {
            return Err(wasi_keyvalue::store::Error::Other(
                "list_keys: cursor not supported".to_owned(),
            ));
        }

        let store = self.get_store_wasi(self_)?;
        let keys = store.get_keys().await.map_err(to_wasi_err)?;
        Ok(wasi_keyvalue::store::KeyResponse { keys, cursor: None })
    }

    async fn drop(&mut self, rep: Resource<Bucket>) -> anyhow::Result<()> {
        self.stores.remove(rep.rep());
        Ok(())
    }
}

pub fn log_error(err: impl std::fmt::Debug) -> Error {
    tracing::warn!("key-value error: {err:?}");
    Error::Other(format!("{err:?}"))
}

use spin_world::v1::key_value::Error as LegacyError;

fn to_legacy_error(value: key_value::Error) -> LegacyError {
    match value {
        Error::StoreTableFull => LegacyError::StoreTableFull,
        Error::NoSuchStore => LegacyError::NoSuchStore,
        Error::AccessDenied => LegacyError::AccessDenied,
        Error::Other(s) => LegacyError::Io(s),
    }
}

#[async_trait]
impl spin_world::v1::key_value::Host for KeyValueDispatch {
    async fn open(&mut self, name: String) -> Result<Result<u32, LegacyError>> {
        let result = <Self as key_value::HostStore>::open(self, name).await?;
        Ok(result.map_err(to_legacy_error).map(|s| s.rep()))
    }

    async fn get(&mut self, store: u32, key: String) -> Result<Result<Vec<u8>, LegacyError>> {
        let this = Resource::new_borrow(store);
        let result = <Self as key_value::HostStore>::get(self, this, key).await?;
        Ok(result
            .map_err(to_legacy_error)
            .and_then(|v| v.ok_or(LegacyError::NoSuchKey)))
    }

    async fn set(
        &mut self,
        store: u32,
        key: String,
        value: Vec<u8>,
    ) -> Result<Result<(), LegacyError>> {
        let this = Resource::new_borrow(store);
        let result = <Self as key_value::HostStore>::set(self, this, key, value).await?;
        Ok(result.map_err(to_legacy_error))
    }

    async fn delete(&mut self, store: u32, key: String) -> Result<Result<(), LegacyError>> {
        let this = Resource::new_borrow(store);
        let result = <Self as key_value::HostStore>::delete(self, this, key).await?;
        Ok(result.map_err(to_legacy_error))
    }

    async fn exists(&mut self, store: u32, key: String) -> Result<Result<bool, LegacyError>> {
        let this = Resource::new_borrow(store);
        let result = <Self as key_value::HostStore>::exists(self, this, key).await?;
        Ok(result.map_err(to_legacy_error))
    }

    async fn get_keys(&mut self, store: u32) -> Result<Result<Vec<String>, LegacyError>> {
        let this = Resource::new_borrow(store);
        let result = <Self as key_value::HostStore>::get_keys(self, this).await?;
        Ok(result.map_err(to_legacy_error))
    }

    async fn close(&mut self, store: u32) -> Result<()> {
        let this = Resource::new_borrow(store);
        <Self as key_value::HostStore>::drop(self, this).await
    }
}
