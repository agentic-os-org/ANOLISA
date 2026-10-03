
/// Versioned envelope for commands sent to an Agent Runtime bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeCommandEnvelope {
    /// Versioned envelope metadata.
    pub header: ContractHeader,
    /// Neutral Runtime command.
    pub command: AgentRuntimeCommand,
}

impl RuntimeCommandEnvelope {
    /// Rejects a header that does not declare the Runtime command schema.
    pub fn validate_schema(&self) -> Result<(), crate::common::EnvelopeSchemaError> {
        self.header.validate_schema(ContractSchema::RuntimeCommand)
    }
}

impl<'de> Deserialize<'de> for RuntimeCommandEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireEnvelope {
            header: ContractHeader,
            command: AgentRuntimeCommand,
        }

        let wire = WireEnvelope::deserialize(deserializer)?;
        let envelope = Self {
            header: wire.header,
            command: wire.command,
        };
        envelope.validate_schema().map_err(de::Error::custom)?;
        Ok(envelope)
    }
}
/// Versioned event emitted by one fenced Agent Runtime binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeEventEnvelope {
    /// Versioned envelope metadata.
    pub header: ContractHeader,
    /// Fenced binding that produced the event.
    pub binding_id: RuntimeBindingId,
    /// Monotonic sequence assigned within the binding.
    pub sequence: u64,
    /// Neutral Runtime event.
    pub event: AgentRuntimeEvent,
}

impl RuntimeEventEnvelope {
    /// Rejects a header that does not declare the Runtime event schema.
    pub fn validate_schema(&self) -> Result<(), crate::common::EnvelopeSchemaError> {
        self.header.validate_schema(ContractSchema::RuntimeEvent)
    }
}

impl<'de> Deserialize<'de> for RuntimeEventEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireEnvelope {
            header: ContractHeader,
            binding_id: RuntimeBindingId,
            sequence: u64,
            event: AgentRuntimeEvent,
        }

        let wire = WireEnvelope::deserialize(deserializer)?;
        let envelope = Self {
            header: wire.header,
            binding_id: wire.binding_id,
            sequence: wire.sequence,
            event: wire.event,
        };
        envelope.validate_schema().map_err(de::Error::custom)?;
        Ok(envelope)
    }
}
