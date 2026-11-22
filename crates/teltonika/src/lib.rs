//! Access to Teltonika devices through the official Web API: a client with login, bulk
//! polling and caching; normalized router (RutOS) and switch (TSWOS) status; on/off control;
//! Event Juggler hook rules; and cable detection between devices.

pub mod client;
pub mod control;
pub mod devices;
pub mod juggler;
pub mod model;
pub mod topology;

pub use client::{Client, Error, Result};
pub use devices::{router_status, switch_status};
pub use topology::find_links;
