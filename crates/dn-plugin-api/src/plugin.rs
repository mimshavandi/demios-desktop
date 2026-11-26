//! The [`Plugin`] trait every add-on implements (ADR-0013 §2).

use crate::error::PluginError;
use crate::host::Host;
use crate::manifest::PluginManifest;
use crate::registrar::Registrar;
use crate::{BoxFuture, Id};

/// An add-on. It is linked into an add-on build and listed in that build's registry; the
/// host calls `register` once at start-up.
///
/// An add-on that fails or panics in `register` isn't loaded, and Settings shows why. A
/// handler that panics fails that call, and the host turns the add-on off until restart.
pub trait Plugin: Send + Sync + 'static {
    /// Who the add-on is and which permissions it asks for.
    fn manifest(&self) -> &PluginManifest;

    /// Declares everything the add-on contributes. Called once.
    fn register(&self, r: &mut Registrar) -> Result<(), PluginError>;

    /// SQL migrations for the add-on's own database file, run in order by the host. They can
    /// only reach that file: `ATTACH` and other schemas are refused.
    fn migrations(&self) -> &'static [&'static str] {
        &[]
    }

    /// Drops everything derived from conversation `id`, which the user purged. The host has
    /// already deleted memory items whose only source was that chat. `host` can only read
    /// and delete the add-on's own data.
    fn forget_conversation<'a>(
        &'a self,
        host: &'a Host,
        id: Id,
    ) -> BoxFuture<'a, Result<(), PluginError>>;

    /// Deletes all the add-on's data, during "Delete all local data". `host` can only read
    /// and delete the add-on's own data. Whether this succeeds or not, the host then deletes
    /// the add-on's database file, folder, settings, secrets and held approvals itself.
    fn delete_all<'a>(&'a self, host: &'a Host) -> BoxFuture<'a, Result<(), PluginError>>;
}
