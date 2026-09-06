ALTER TABLE directories
    ADD COLUMN deadline DATE;

ALTER TABLE task_groups
    ADD COLUMN deadline DATE;

ALTER TABLE tasks
    ADD COLUMN deadline DATE;

ALTER TABLE directories
    DROP COLUMN priority;

ALTER TABLE task_groups
    DROP COLUMN priority;

ALTER TABLE tasks
    DROP COLUMN priority;
