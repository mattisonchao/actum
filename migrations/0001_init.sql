CREATE TABLE directories (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    parent_id BIGINT REFERENCES directories(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (name <> '' AND name !~ '/'),
    priority SMALLINT CHECK (priority BETWEEN 1 AND 3),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX directories_root_name_key
    ON directories (name)
    WHERE parent_id IS NULL;

CREATE UNIQUE INDEX directories_parent_name_key
    ON directories (parent_id, name)
    WHERE parent_id IS NOT NULL;

CREATE TABLE task_groups (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    directory_id BIGINT NOT NULL REFERENCES directories(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (name <> ''),
    priority SMALLINT CHECK (priority BETWEEN 1 AND 3),
    links TEXT[] NOT NULL DEFAULT '{}',
    position INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (directory_id, name),
    UNIQUE (id, directory_id)
);

CREATE TABLE tasks (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    directory_id BIGINT NOT NULL REFERENCES directories(id) ON DELETE CASCADE,
    group_id BIGINT,
    title TEXT NOT NULL CHECK (title <> ''),
    priority SMALLINT CHECK (priority BETWEEN 1 AND 3),
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'completed')),
    links TEXT[] NOT NULL DEFAULT '{}',
    position INTEGER NOT NULL DEFAULT 0,
    completion_note TEXT,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (directory_id, title),
    FOREIGN KEY (group_id, directory_id)
        REFERENCES task_groups(id, directory_id)
        ON DELETE SET NULL (group_id)
);

CREATE INDEX tasks_directory_status_idx ON tasks (directory_id, status);
CREATE INDEX tasks_group_position_idx ON tasks (group_id, position, id);

CREATE FUNCTION actum_touch_updated_at() RETURNS trigger AS $$
BEGIN
    NEW.updated_at = now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER directories_touch_updated_at
    BEFORE UPDATE ON directories
    FOR EACH ROW EXECUTE FUNCTION actum_touch_updated_at();

CREATE TRIGGER task_groups_touch_updated_at
    BEFORE UPDATE ON task_groups
    FOR EACH ROW EXECUTE FUNCTION actum_touch_updated_at();

CREATE TRIGGER tasks_touch_updated_at
    BEFORE UPDATE ON tasks
    FOR EACH ROW EXECUTE FUNCTION actum_touch_updated_at();
