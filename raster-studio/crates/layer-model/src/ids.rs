//! Strongly-typed identifiers. Using distinct newtypes prevents accidentally
//! passing a `MaskId` where a `LayerId` is expected.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Generate a fresh random id.
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_type!(
    /// Identity of a layer within a document.
    LayerId
);
id_type!(
    /// Identity of a mask attached to a layer.
    MaskId
);
id_type!(
    /// Identity of an asset (embedded or linked) in the asset store.
    AssetId
);
