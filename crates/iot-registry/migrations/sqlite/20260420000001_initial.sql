-- iot-registry — initial schema (SQLite dialect).
-- ADR-0007: forward-only; never edit this file after the first release that shipped it.

CREATE TABLE IF NOT EXISTS devices (
    id                  TEXT NOT NULL PRIMARY KEY,     -- ULID, 26 chars
    integration         TEXT NOT NULL,                 -- plugin id that owns the device
    external_id         TEXT NOT NULL,
    manufacturer        TEXT NOT NULL DEFAULT '',
    model               TEXT NOT NULL DEFAULT '',
    label               TEXT NOT NULL DEFAULT '',
    trust_level         TEXT NOT NULL DEFAULT 'user_added', -- 'discovered' | 'user_added' | 'verified'
    schema_version      INTEGER NOT NULL DEFAULT 1,
    plugin_meta_json    TEXT NOT NULL DEFAULT '{}',
    last_seen           TEXT NOT NULL,                 -- RFC3339
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    UNIQUE (integration, external_id)
);

CREATE INDEX IF NOT EXISTS idx_devices_integration ON devices(integration);
CREATE INDEX IF NOT EXISTS idx_devices_last_seen ON devices(last_seen);

CREATE TABLE IF NOT EXISTS entities (
    id                  TEXT NOT NULL PRIMARY KEY,     -- ULID
    device_id           TEXT NOT NULL,
    type                TEXT NOT NULL,                 -- "sensor.temperature", "switch", ...
    unit                TEXT NOT NULL DEFAULT '',
    rw                  TEXT NOT NULL DEFAULT 'read',  -- 'read' | 'write' | 'read_write'
    device_class        TEXT NOT NULL DEFAULT '',
    meta_json           TEXT NOT NULL DEFAULT '{}',
    FOREIGN KEY (device_id) REFERENCES devices(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_entities_device ON entities(device_id);
CREATE INDEX IF NOT EXISTS idx_entities_type ON entities(type);

CREATE TABLE IF NOT EXISTS device_rooms (
    device_id           TEXT NOT NULL,
    room                TEXT NOT NULL,
    PRIMARY KEY (device_id, room),
    FOREIGN KEY (device_id) REFERENCES devices(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS device_capabilities (
    device_id           TEXT NOT NULL,
    capability          TEXT NOT NULL,
    PRIMARY KEY (device_id, capability),
    FOREIGN KEY (device_id) REFERENCES devices(id) ON DELETE CASCADE
);
