-- M7.2: append-only audit trail for thought rescoping.
--
-- Reclassifies thoughts.scope as mutable *operational metadata* (DESIGN.md
-- §10 amendment, docs/milestones/m7.2-thought-rescoping.md). Written only by
-- the rescope storage helpers; never updated, never deleted. One row per
-- (thought, scope change) event — repeat rescopes append, they do not
-- overwrite (contrast: a state-flip column would record only the last hop;
-- the 0010 migration_audit event-table pattern is the precedent).

CREATE TABLE scope_changes (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    thought_id  UUID        NOT NULL REFERENCES thoughts(id) ON DELETE CASCADE,
    old_scope   TEXT        NOT NULL,
    new_scope   TEXT        NOT NULL,
    changed_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- actor follows the thoughts.source convention (free text: 'manual',
    -- 'agent:claude-code'). Transport is not identity: a CHECK-constrained
    -- 'cli'/'mcp' would make every agent indistinguishable, and the trail's
    -- reason for existing is naming WHICH agent moved a thought (the
    -- 2026-06-17 decision-record misfiled-write case).
    actor       TEXT        NOT NULL,
    reason      TEXT
);

-- Audit-path lookup ("has this thought been re-filed?") and recent-changes
-- diagnostics, mirroring migration_audit's ran_at DESC pattern.
CREATE INDEX scope_changes_thought_idx
    ON scope_changes (thought_id, changed_at DESC);
CREATE INDEX scope_changes_changed_at_idx
    ON scope_changes (changed_at DESC);

-- House rule (M5.2): every migration seeds its audit row.
INSERT INTO migration_audit (migration, rows_touched, notes) VALUES
    ('0012_thought_rescoping', 0,
     'Created scope_changes append-only audit table. Reclassifies thoughts.scope as mutable operational metadata (docs/milestones/m7.2-thought-rescoping.md). No thoughts rows touched.');
