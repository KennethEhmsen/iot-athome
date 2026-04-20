-- iot-registry — initial schema (PostgreSQL dialect).
-- ADR-0007: forward-only; never edit this file after the first release that shipped it.

CREATE TABLE IF NOT EXISTS devices (
    id                  TEXT NOT NULL PRIMARY KEY,
    integration         TEXT NOT NULL,
    external_id         TEXT NOT NULL,
    manufacturer        TEXT NOT NULL DEFAULT '',
    model               TEXT NOT NULL DEFAULT '',
    label               TEXT NOT NULL DEFAULT '',
    trust_level         TEXT NOT NULL DEFAULT 'user_added'
                           CHECK (trust_level IN ('discovered', 'user_added', 'verified')),
    schema_version      INTEGER NOT NULL DEFAULT 1,
    plugin_meta         JSONB NOT NULL DEFAULT '{}'::jsonb,
    last_seen           TIMESTAMPTZ NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (integration, external_id)
);

CREATE INDEX IF NOT EXISTS idx_devices_integration ON devices(integration);
CREATE INDEX IF NOT EXISTS idx_devices_last_seen ON devices(last_seen);

CREATE TABLE IF NOT EXISTS entities (
    id                  TEXT NOT NULL PRIMARY KEY,
    device_id           TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    type                TEXT NOT NULL,
    unit                TEXT NOT NULL DEFAULT '',
    rw                  TEXT NOT NULL DEFAULT 'read'
                           CHECK (rw IN ('read', 'write', 'read_write')),
    device_class        TEXT NOT NULL DEFAULT '',
    meta                JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX IF NOT EXISTS idx_entities_device ON entities(device_id);
CREATE INDEX IF NOT EXISTS idx_entities_type ON entities(type);

CREATE TABLE IF NOT EXISTS device_rooms (
    device_id           TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    room                TEXT NOT NULL,
    PRIMARY KEY (device_id, room)
);

CREATE TABLE IF NOT EXISTS device_capabilities (
    device_id           TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    capability          TEXT NOT NULL,
    PRIMARY KEY (device_id, capability)
);
