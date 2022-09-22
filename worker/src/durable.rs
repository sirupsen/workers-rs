//! Durable Objects provide low-latency coordination and consistent storage for the Workers platform.
//! A given namespace can support essentially unlimited Durable Objects, with each Object having
//! access to a transactional, key-value storage API.
//!
//! Durable Objects consist of two components: a class that defines a template for creating Durable
//! Objects and a Workers script that instantiates and uses those Durable Objects.
//!
//! The class and the Workers script are linked together with a binding.
//!
//! [Learn more](https://developers.cloudflare.com/workers/learning/using-durable-objects) about
//! using Durable Objects.

use std::{ops::Deref, time::Duration};

use crate::{
    date::Date,
    env::{Env, EnvBinding},
    error::Error,
    request::Request,
    response::Response,
    Result,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use js_sys::{Map, Number, Object};
use serde::{Serialize, de::DeserializeOwned};
use wasm_bindgen::{prelude::*, JsCast};
use worker_sys::{
    durable_object::{
        JsObjectId, ObjectNamespace as EdgeObjectNamespace, ObjectState, ObjectStorage, ObjectStub,
        ObjectTransaction,
    },
    Response as EdgeResponse,
};
// use wasm_bindgen_futures::future_to_promise;
use wasm_bindgen_futures::JsFuture;

/// A Durable Object stub is a client object used to send requests to a remote Durable Object.
pub struct Stub {
    inner: ObjectStub,
}

impl Stub {
    /// Send an internal Request to the Durable Object to which the stub points.
    pub async fn fetch_with_request(&self, req: Request) -> Result<Response> {
        let promise = self.inner.fetch_with_request_internal(req.inner());
        let response = JsFuture::from(promise).await?;
        Ok(response.dyn_into::<EdgeResponse>()?.into())
    }

    /// Construct a Request from a URL to the Durable Object to which the stub points.
    pub async fn fetch_with_str(&self, url: &str) -> Result<Response> {
        let promise = self.inner.fetch_with_str_internal(url);
        let response = JsFuture::from(promise).await?;
        Ok(response.dyn_into::<EdgeResponse>()?.into())
    }
}

/// Use an ObjectNamespace to get access to Stubs for communication with a Durable Object instance.
/// A given namespace can support essentially unlimited Durable Objects, with each Object having
/// access to a transactional, key-value storage API.
pub struct ObjectNamespace {
    inner: EdgeObjectNamespace,
}

impl ObjectNamespace {
    /// This method derives a unique object ID from the given name string. It will always return the
    /// same ID when given the same name as input.
    pub fn id_from_name(&self, name: &str) -> Result<ObjectId> {
        self.inner
            .id_from_name_internal(name)
            .map_err(Error::from)
            .map(|id| ObjectId {
                inner: id,
                namespace: Some(self),
            })
    }

    /// This method parses an ID that was previously stringified. This is useful in particular with
    /// IDs created using `unique_id(&self)`, as these IDs need to be stored somewhere, probably as
    // as a string.
    ///
    /// A stringified object ID is a 64-digit hexadecimal number. However, not all 64-digit hex
    /// numbers are valid IDs. This method will throw if it is passed an ID that was not originally
    /// created by newUniqueId() or idFromName(). It will also throw if the ID was originally
    /// created for a different namespace.
    pub fn id_from_string(&self, hex_id: &str) -> Result<ObjectId> {
        self.inner
            .id_from_string_internal(hex_id)
            .map_err(Error::from)
            .map(|id| ObjectId {
                inner: id,
                namespace: Some(self),
            })
    }

    /// Creates a new object ID randomly. This method will never return the same ID twice, and thus
    /// it is guaranteed that the object does not yet exist and has never existed at the time the
    /// method returns.
    pub fn unique_id(&self) -> Result<ObjectId> {
        self.inner
            .new_unique_id_internal()
            .map_err(Error::from)
            .map(|id| ObjectId {
                inner: id,
                namespace: Some(self),
            })
    }

    /// Durable Objects can be created so that they only run and store data within a specific
    /// jurisdiction to comply with local regulations. You must specify the jurisdiction when
    /// generating the Durable Object's id.
    ///
    /// Jurisdiction constraints can only be used with ids created by `unique_id()` and are not
    /// currently compatible with ids created by `id_from_name()`.
    ///
    /// See supported jurisdictions and more documentation at:
    /// <https://developers.cloudflare.com/workers/runtime-apis/durable-objects#restricting-objects-to-a-jurisdiction>
    pub fn unique_id_with_jurisdiction(&self, jd: &str) -> Result<ObjectId> {
        let options = Object::new();
        js_sys::Reflect::set(&options, &JsValue::from("jurisdiction"), &jd.into())?;
        self.inner
            .new_unique_id_with_options_internal(&options)
            .map_err(Error::from)
            .map(|id| ObjectId {
                inner: id,
                namespace: Some(self),
            })
    }
}

/// An ObjectId is used to identify, locate, and access a Durable Object via interaction with its
/// Stub.
pub struct ObjectId<'a> {
    inner: JsObjectId,
    namespace: Option<&'a ObjectNamespace>,
}

impl ObjectId<'_> {
    /// Get a Stub for the Durable Object instance identified by this ObjectId.
    pub fn get_stub(&self) -> Result<Stub> {
        self.namespace
            .ok_or_else(|| JsValue::from("Cannot get stub from within a Durable Object"))
            .and_then(|n| {
                Ok(Stub {
                    inner: n.inner.get_internal(&self.inner)?,
                })
            })
            .map_err(Error::from)
    }
}

impl ToString for ObjectId<'_> {
    fn to_string(&self) -> String {
        self.inner.to_string().into()
    }
}

/// Passed from the runtime to provide access to the Durable Object's storage as well as various
/// metadata about the Object.
pub struct State {
    inner: ObjectState,
}

impl State {
    /// The ID of this Durable Object which can be converted into a hex string using its `to_string()`
    /// method.
    pub fn id(&self) -> ObjectId<'_> {
        ObjectId {
            inner: self.inner.id_internal(),
            namespace: None,
        }
    }

    /// Contains methods for accessing persistent storage via the transactional storage API. See
    /// [Transactional Storage API](https://developers.cloudflare.com/workers/runtime-apis/durable-objects#transactional-storage-api) for a detailed reference.
    pub fn storage(&self) -> Storage {
        Storage {
            inner: self.inner.storage_internal(),
        }
    }

    // needs to be accessed by the `durable_object` macro in a conversion step
    pub fn _inner(self) -> ObjectState {
        self.inner
    }
}

impl From<ObjectState> for State {
    fn from(o: ObjectState) -> Self {
        Self { inner: o }
    }
}

/// Access a Durable Object's Storage API. Each method is implicitly wrapped inside a transaction,
/// such that its results are atomic and isolated from all other storage operations, even when
/// accessing multiple key-value pairs.
pub struct Storage {
    inner: ObjectStorage,
}

impl Storage {
    /// Retrieves the value associated with the given key. The type of the returned value will be
    /// whatever was previously written for the key, or undefined if the key does not exist.
    pub async fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<T> {
        JsFuture::from(self.inner.get_internal(key)?)
            .await
            .and_then(|val| {
                if val.is_undefined() {
                    Err(JsValue::from("No such value in storage."))
                } else {
                    serde_wasm_bindgen::from_value(val).map_err(|e| JsValue::from(e.to_string()))
                }
            })
            .map_err(Error::from)
    }

    /// Retrieves the values associated with each of the provided keys.
    pub async fn get_multiple(&self, keys: Vec<impl Deref<Target = str>>) -> Result<Map> {
        let keys = self.inner.get_multiple_internal(
            keys.into_iter()
                .map(|key| JsValue::from(key.deref()))
                .collect(),
        )?;
        let keys = JsFuture::from(keys).await?;
        keys.dyn_into::<Map>().map_err(Error::from)
    }

    /// Stores the value and associates it with the given key.
    pub async fn put<T: Serialize>(&mut self, key: &str, value: T) -> Result<()> {
        JsFuture::from(self.inner.put_internal(key, serde_wasm_bindgen::to_value(&value)?)?)
            .await
            .map_err(Error::from)
            .map(|_| ())
    }

    /// Takes a serializable struct and stores each of its keys and values to storage.
    pub async fn put_multiple<T: Serialize>(&mut self, values: T) -> Result<()> {
        let values = serde_wasm_bindgen::to_value(&values)?;
        if !values.is_object() {
            return Err("Must pass in a struct type".to_string().into());
        }
        JsFuture::from(self.inner.put_multiple_internal(values)?)
            .await
            .map_err(Error::from)
            .map(|_| ())
    }

    /// Deletes the key and associated value. Returns true if the key existed or false if it didn't.
    pub async fn delete(&mut self, key: &str) -> Result<bool> {
        let fut: JsFuture = self.inner.delete_internal(key)?.into();
        fut.await
            .and_then(|jsv| {
                jsv.as_bool()
                    .ok_or_else(|| JsValue::from("Promise did not return bool"))
            })
            .map_err(Error::from)
    }

    /// Deletes the provided keys and their associated values. Returns a count of the number of
    /// key-value pairs deleted.
    pub async fn delete_multiple(&mut self, keys: Vec<impl Deref<Target = str>>) -> Result<usize> {
        let fut: JsFuture = self
            .inner
            .delete_multiple_internal(
                keys.into_iter()
                    .map(|key| JsValue::from(key.deref()))
                    .collect(),
            )?
            .into();
        fut.await
            .and_then(|jsv| {
                jsv.as_f64()
                    .map(|f| f as usize)
                    .ok_or_else(|| JsValue::from("Promise did not return number"))
            })
            .map_err(Error::from)
    }

    /// Deletes all keys and associated values, effectively deallocating all storage used by the
    /// Durable Object. In the event of a failure while the operation is still in flight, it may be
    /// that only a subset of the data is properly deleted.
    pub async fn delete_all(&mut self) -> Result<()> {
        let fut: JsFuture = self.inner.delete_all_internal()?.into();
        fut.await.map(|_| ()).map_err(Error::from)
    }

    /// Returns all keys and values associated with the current Durable Object in ascending
    /// lexicographic sorted order.
    ///
    /// Be aware of how much data may be stored in your Durable Object before calling this version
    /// of list without options, because it will all be loaded into the Durable Object's memory,
    /// potentially hitting its [limit](https://developers.cloudflare.com/workers/platform/limits#durable-objects-limits).
    /// If that is a concern, use the alternate `list_with_options()` method.
    pub async fn list(&self) -> Result<Map> {
        let fut: JsFuture = self.inner.list_internal()?.into();
        fut.await
            .and_then(|jsv| jsv.dyn_into())
            .map_err(Error::from)
    }

    /// Returns keys associated with the current Durable Object according to the parameters in the
    /// provided options object.
    pub async fn list_with_options(&self, opts: ListOptions<'_>) -> Result<Map> {
        let fut: JsFuture = self
            .inner
            .list_with_options_internal(serde_wasm_bindgen::to_value(&opts)?.into())?
            .into();
        fut.await
            .and_then(|jsv| jsv.dyn_into())
            .map_err(Error::from)
    }

    /// Retrieves the current alarm time (if set) as integer milliseconds since epoch.
    /// The alarm is considered to be set if it has not started, or if it has failed
    /// and any retry has not begun. If no alarm is set, `get_alarm()` returns `None`.
    pub async fn get_alarm(&self) -> Result<Option<i64>> {
        let fut: JsFuture = self.inner.get_alarm_internal(JsValue::NULL.into())?.into();
        fut.await
            .map(|jsv| jsv.as_f64().map(|f| f as i64))
            .map_err(Error::from)
    }

    pub async fn get_alarm_with_options(&self, options: GetAlarmOptions) -> Result<Option<i64>> {
        let fut: JsFuture = self
            .inner
            .get_alarm_internal(serde_wasm_bindgen::to_value(&options)?.into())?
            .into();
        fut.await
            .map(|jsv| jsv.as_f64().map(|f| f as i64))
            .map_err(Error::from)
    }

    /// Sets the current alarm time to the given datetime.
    ///
    /// If `set_alarm()` is called with a time equal to or before Date.now(), the alarm
    /// will be scheduled for asynchronous execution in the immediate future. If the
    /// alarm handler is currently executing in this case, it will not be canceled.
    /// Alarms can be set to millisecond granularity and will usually execute within
    /// a few milliseconds after the set time, but can be delayed by up to a minute
    /// due to maintenance or failures while failover takes place.
    pub async fn set_alarm(&self, scheduled_time: impl Into<ScheduledTime>) -> Result<()> {
        let fut: JsFuture = self
            .inner
            .set_alarm_internal(scheduled_time.into().schedule(), JsValue::NULL.into())?
            .into();
        fut.await.map(|_| ()).map_err(Error::from)
    }

    pub async fn set_alarm_with_options(
        &self,
        scheduled_time: impl Into<ScheduledTime>,
        options: SetAlarmOptions,
    ) -> Result<()> {
        let fut: JsFuture = self
            .inner
            .set_alarm_internal(
                scheduled_time.into().schedule(),
                serde_wasm_bindgen::to_value(&options)?.into(),
            )?
            .into();
        fut.await.map(|_| ()).map_err(Error::from)
    }

    /// Deletes the alarm if one exists. Does not cancel the alarm handler if it is
    /// currently executing.
    pub async fn delete_alarm(&self) -> Result<()> {
        let fut: JsFuture = self
            .inner
            .delete_alarm_internal(JsValue::NULL.into())?
            .into();
        fut.await.map(|_| ()).map_err(Error::from)
    }

    pub async fn delete_alarm_with_options(&self, options: SetAlarmOptions) -> Result<()> {
        let fut: JsFuture = self
            .inner
            .delete_alarm_internal(serde_wasm_bindgen::to_value(&options)?.into())?
            .into();
        fut.await.map(|_| ()).map_err(Error::from)
    }

    // TODO(nilslice): follow up with runtime team on transaction API in general
    // This function doesn't work on stable yet because the wasm_bindgen `Closure` type is still nightly-gated
    // #[allow(dead_code)]
    // async fn transaction<F>(&mut self, closure: fn(Transaction) -> F) -> Result<()>
    // where
    //     F: Future<Output = Result<()>> + 'static,
    // {
    //     let mut clos = |t: Transaction| {
    //         future_to_promise(async move {
    //             closure(t)
    //                 .await
    //                 .map_err(JsValue::from)
    //                 .map(|_| JsValue::NULL)
    //         })
    //     };
    //     JsFuture::from(self.inner.transaction_internal(&mut clos)?)
    //         .await
    //         .map_err(Error::from)
    //         .map(|_| ())
    // }
}

#[allow(dead_code)]
struct Transaction {
    inner: ObjectTransaction,
}

#[allow(dead_code)]
impl Transaction {
    async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<T> {
        JsFuture::from(self.inner.get_internal(key)?)
            .await
            .and_then(|val| {
                if val.is_undefined() {
                    Err(JsValue::from("No such value in storage."))
                } else {
                    serde_wasm_bindgen::from_value(val).map_err(std::convert::Into::into)
                }
            })
            .map_err(Error::from)
    }

    async fn get_multiple(&self, keys: Vec<impl Deref<Target = str>>) -> Result<Map> {
        let keys = self.inner.get_multiple_internal(
            keys.into_iter()
                .map(|key| JsValue::from(key.deref()))
                .collect(),
        )?;
        let keys = JsFuture::from(keys).await?;
        keys.dyn_into::<Map>().map_err(Error::from)
    }

    async fn put<T: Serialize>(&mut self, key: &str, value: T) -> Result<()> {
        JsFuture::from(self.inner.put_internal(key, serde_wasm_bindgen::to_value(&value)?)?)
            .await
            .map_err(Error::from)
            .map(|_| ())
    }

    // Each key-value pair in the serialized object will be added to the storage
    async fn put_multiple<T: Serialize>(&mut self, values: T) -> Result<()> {
        let values = serde_wasm_bindgen::to_value(&values)?;
        if !values.is_object() {
            return Err("Must pass in a struct type".to_string().into());
        }
        JsFuture::from(self.inner.put_multiple_internal(values)?)
            .await
            .map_err(Error::from)
            .map(|_| ())
    }

    async fn delete(&mut self, key: &str) -> Result<bool> {
        let fut: JsFuture = self.inner.delete_internal(key)?.into();
        fut.await
            .and_then(|jsv| {
                jsv.as_bool()
                    .ok_or_else(|| JsValue::from("Promise did not return bool"))
            })
            .map_err(Error::from)
    }

    async fn delete_multiple(&mut self, keys: Vec<impl Deref<Target = str>>) -> Result<usize> {
        let fut: JsFuture = self
            .inner
            .delete_multiple_internal(
                keys.into_iter()
                    .map(|key| JsValue::from(key.deref()))
                    .collect(),
            )?
            .into();
        fut.await
            .and_then(|jsv| {
                jsv.as_f64()
                    .map(|f| f as usize)
                    .ok_or_else(|| JsValue::from("Promise did not return number"))
            })
            .map_err(Error::from)
    }

    async fn delete_all(&mut self) -> Result<()> {
        let fut: JsFuture = self.inner.delete_all_internal()?.into();
        fut.await.map(|_| ()).map_err(Error::from)
    }

    async fn list(&self) -> Result<Map> {
        let fut: JsFuture = self.inner.list_internal()?.into();
        fut.await
            .and_then(|jsv| jsv.dyn_into())
            .map_err(Error::from)
    }

    async fn list_with_options(&self, opts: ListOptions<'_>) -> Result<Map> {
        let fut: JsFuture = self
            .inner
            .list_with_options_internal(serde_wasm_bindgen::to_value(&opts)?.into())?
            .into();
        fut.await
            .and_then(|jsv| jsv.dyn_into())
            .map_err(Error::from)
    }

    fn rollback(&mut self) -> Result<()> {
        self.inner.rollback_internal().map_err(Error::from)
    }
}

#[derive(Default, Serialize)]
pub struct ListOptions<'a> {
    /// Key at which the list results should start, inclusive.
    #[serde(skip_serializing_if = "Option::is_none")]
    start: Option<&'a str>,
    /// Key at which the list results should end, exclusive.
    #[serde(skip_serializing_if = "Option::is_none")]
    end: Option<&'a str>,
    /// Restricts results to only include key-value pairs whose keys begin with the prefix.
    #[serde(skip_serializing_if = "Option::is_none")]
    prefix: Option<&'a str>,
    /// If true, return results in descending lexicographic order instead of the default ascending
    /// order.
    #[serde(skip_serializing_if = "Option::is_none")]
    reverse: Option<bool>,
    /// Maximum number of key-value pairs to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

impl<'a> ListOptions<'a> {
    /// Create a new ListOptions struct with no options set.
    pub fn new() -> Self {
        Default::default()
    }

    /// Key at which the list results should start, inclusive.
    pub fn start(mut self, val: &'a str) -> Self {
        self.start = Some(val);
        self
    }

    /// Key at which the list results should end, exclusive.
    pub fn end(mut self, val: &'a str) -> Self {
        self.end = Some(val);
        self
    }

    /// Restricts results to only include key-value pairs whose keys begin with the prefix.
    pub fn prefix(mut self, val: &'a str) -> Self {
        self.prefix = Some(val);
        self
    }

    /// If true, return results in descending lexicographic order instead of the default ascending
    /// order.
    pub fn reverse(mut self, val: bool) -> Self {
        self.reverse = Some(val);
        self
    }

    /// Maximum number of key-value pairs to return.
    pub fn limit(mut self, val: usize) -> Self {
        self.limit = Some(val);
        self
    }
}

enum ScheduledTimeInit {
    Date(js_sys::Date),
    Offset(f64),
}

/// Determines when a Durable Object alarm should be ran, based on a timestamp or offset.
///
/// Implements [`From`] for:
/// - [`Duration`], interpreted as an offset.
/// - [`i64`], interpreted as an offset.
/// - [`DateTime`], interpreted as a timestamp.
///
/// When an offset is used, the time at which `set_alarm()` or `set_alarm_with_options()` is called
/// is used to compute the scheduled time. [`Date::now`] is used as the current time.
pub struct ScheduledTime {
    init: ScheduledTimeInit,
}

impl ScheduledTime {
    pub fn new(date: js_sys::Date) -> Self {
        Self {
            init: ScheduledTimeInit::Date(date),
        }
    }

    fn schedule(self) -> js_sys::Date {
        match self.init {
            ScheduledTimeInit::Date(date) => date,
            ScheduledTimeInit::Offset(offset) => {
                let now = Date::now().as_millis() as f64;
                js_sys::Date::new(&Number::from(now + offset))
            }
        }
    }
}

impl From<i64> for ScheduledTime {
    fn from(offset: i64) -> Self {
        ScheduledTime {
            init: ScheduledTimeInit::Offset(offset as f64),
        }
    }
}

impl From<DateTime<Utc>> for ScheduledTime {
    fn from(date: DateTime<Utc>) -> Self {
        ScheduledTime {
            init: ScheduledTimeInit::Date(js_sys::Date::new(&Number::from(
                date.timestamp_millis() as f64,
            ))),
        }
    }
}

impl From<Duration> for ScheduledTime {
    fn from(offset: Duration) -> Self {
        ScheduledTime {
            init: ScheduledTimeInit::Offset(offset.as_millis() as f64),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GetAlarmOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_concurrency: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SetAlarmOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_concurrency: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_unconfirmed: Option<bool>,
}

impl EnvBinding for ObjectNamespace {
    const TYPE_NAME: &'static str = "DurableObjectNamespace";
}

impl JsCast for ObjectNamespace {
    fn instanceof(val: &JsValue) -> bool {
        val.is_instance_of::<EdgeObjectNamespace>()
    }

    fn unchecked_from_js(val: JsValue) -> Self {
        Self { inner: val.into() }
    }

    fn unchecked_from_js_ref(val: &JsValue) -> &Self {
        unsafe { &*(val as *const JsValue as *const Self) }
    }
}

impl From<ObjectNamespace> for JsValue {
    fn from(ns: ObjectNamespace) -> Self {
        JsValue::from(ns.inner)
    }
}

impl AsRef<JsValue> for ObjectNamespace {
    fn as_ref(&self) -> &JsValue {
        &self.inner
    }
}

/**
**Note:** Implement this trait with a standard `impl DurableObject for YourType` block, but in order to
integrate them with the Workers Runtime, you must also add the **`#[durable_object]`** attribute
macro to both the impl block and the struct type definition.

## Example
```no_run
use worker::*;

#[durable_object]
pub struct Chatroom {
    users: Vec<User>,
    messages: Vec<Message>
    state: State,
    env: Env, // access `Env` across requests, use inside `fetch`

}

#[durable_object]
impl DurableObject for Chatroom {
    fn new(state: State, env: Env) -> Self {
        Self {
            users: vec![],
            messages: vec![],
            state: state,
            env,
        }
    }

    async fn fetch(&mut self, _req: Request) -> Result<Response> {
        // do some work when a worker makes a request to this DO
        Response::ok(&format!("{} active users.", self.users.len()))
    }
}
```
*/
#[async_trait(?Send)]
pub trait DurableObject {
    fn new(state: State, env: Env) -> Self;
    async fn fetch(&mut self, req: Request) -> Result<Response>;
    async fn alarm(&mut self) -> Result<Response> {
        unimplemented!("alarm() handler not implemented")
    }
}
