impl SqliteTaskStore {
    /// Creates and durably publishes a verified online backup.
    ///
    /// The destination must be an absolute, previously unused path beneath a
    /// private directory. The source writer is exclusively borrowed while the
    /// SQLite online backup captures committed WAL state.
    ///
    /// # Errors
    ///
    /// Returns an error when path hardening, online copy, schema validation,
    /// installation binding, file sync, or atomic publication fails.
    pub fn backup_to_verified(
        &mut self,
        destination: impl AsRef<Path>,
        expected_installation_id: &InstallationId,
    ) -> Result<(), StoreError> {
        let destination = destination.as_ref();
        sqlite::prepare_new_private_file_path(destination)?;
        verify_connection(self.connection(), expected_installation_id)?;

        let temporary = TemporaryDatabase::create(destination)?;
        {
            let mut destination_connection = open_temporary_database(temporary.path())?;
            online_copy(self.connection(), &mut destination_connection)?;
            configure_standalone_database(&destination_connection)?;
        }
        require_self_contained_database(temporary.path())?;
        verify_backup_path(temporary.path(), expected_installation_id)?;
        temporary.publish(destination)
    }

    /// Verifies a private backup without modifying or migrating it.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe paths, malformed or incompatible schemas,
    /// failed integrity checks, or an installation identity mismatch.
    pub fn verify_backup(
        backup_path: impl AsRef<Path>,
        expected_installation_id: &InstallationId,
    ) -> Result<(), StoreError> {
        verify_backup_path(backup_path.as_ref(), expected_installation_id)
    }

    /// Restores a verified backup to a new private database path.
    ///
    /// The destination is never opened or overwritten when it already exists.
    /// Known older schemas are migrated on the temporary copy before atomic
    /// publication; the source backup remains read-only.
    ///
    /// # Errors
    ///
    /// Returns an error when verification, online copy, migration, durability,
    /// publication, or final store open fails.
    pub fn restore_to_new_path(
        backup_path: impl AsRef<Path>,
        destination: impl AsRef<Path>,
        expected_installation_id: &InstallationId,
    ) -> Result<Self, StoreError> {
        let backup_path = backup_path.as_ref();
        let destination = destination.as_ref();
        verify_backup_path(backup_path, expected_installation_id)?;
        sqlite::prepare_new_private_file_path(destination)?;

        let source = open_read_only_database(backup_path)?;
        let temporary = TemporaryDatabase::create(destination)?;
        {
            let mut destination_connection = open_temporary_database(temporary.path())?;
            online_copy(&source, &mut destination_connection)?;
            configure_standalone_database(&destination_connection)?;
            schema::migrate(&mut destination_connection)?;
            bind_restored_installation(&destination_connection, expected_installation_id)?;
            verify_connection(&destination_connection, expected_installation_id)?;
        }
        require_self_contained_database(temporary.path())?;
        temporary.publish(destination)?;

        let store = Self::open(destination)?;
        verify_connection(store.connection(), expected_installation_id)?;
        Ok(store)
    }
}
