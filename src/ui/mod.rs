pub mod console;
pub mod dashboard;
pub mod discover;
pub mod indices;
pub mod profiles;
pub mod streams;

use std::sync::Arc;

use serde_json::Value;

use crate::es::Conn;
use crate::es::catalog::Catalog;
use crate::profile::{Profile, Store};

/// Konteks yang diberikan shell ke setiap modul untuk profile aktif.
pub struct Ctx<'a> {
    pub conn: &'a Arc<Conn>,
    pub profile: &'a Profile,
    pub catalog: &'a mut Catalog,
    pub caps: Caps,
    pub store: &'a Store,
    /// Semua profile (untuk target lintas cluster, mis. tab Console).
    pub profiles: &'a [Profile],
    pub nav: &'a mut Option<Nav>,
}

/// Kemampuan cluster dari `GET /` (PRF-11). Fitur yang tidak didukung disembunyikan.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    pub major: u32,
    pub minor: u32,
    /// Elasticsearch Serverless: API operasional (shard, node, ILM, health) tidak tersedia.
    pub serverless: bool,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            major: 9,
            minor: 0,
            serverless: false,
        }
    }
}

impl Caps {
    pub fn from_version(version: &str, flavor: &str) -> Self {
        let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
        Self {
            major: parts.next().filter(|m| *m > 0).unwrap_or(8),
            minor: parts.next().unwrap_or(0),
            serverless: flavor == "serverless",
        }
    }

    /// ES|QL (`POST /_query`) tersedia sejak 8.11.
    pub fn esql(&self) -> bool {
        self.serverless || self.major > 8 || (self.major == 8 && self.minor >= 11)
    }

    /// Shard, ILM, cluster health, dan API node.
    pub fn cluster_ops(&self) -> bool {
        !self.serverless
    }
}

/// Permintaan pindah modul dari modul lain.
pub enum Nav {
    Console {
        method: String,
        path: String,
        body: Option<Value>,
    },
    Discover {
        pattern: String,
    },
}
