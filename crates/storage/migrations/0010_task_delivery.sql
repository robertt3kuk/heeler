-- Schema v10: the delivery facet of a task (slice DL-1, protocol 0.25).
--
-- A task's run state (`state`) says what the agent did; `delivery` says where its finished work
-- is: awaiting review, accepted, changes requested, merged or discarded. `delivery_json` keeps
-- the last 20 moves (who, when, the note of a change request, the merge commit and strategy).
-- `after_policy` is what the task waits for in each of its dependencies: their run finishing
-- (`done`, the behaviour before this schema), a person's acceptance or the merge of their
-- branch.
--
-- Existing tasks keep today's behaviour: no delivery, policy `done`.
ALTER TABLE tasks ADD COLUMN delivery TEXT NOT NULL DEFAULT 'none'
    CHECK (delivery IN ('none', 'awaiting_review', 'accepted', 'changes_requested',
                        'merged', 'discarded'));
ALTER TABLE tasks ADD COLUMN after_policy TEXT NOT NULL DEFAULT 'done'
    CHECK (after_policy IN ('done', 'accepted', 'merged'));
ALTER TABLE tasks ADD COLUMN delivery_json TEXT NOT NULL DEFAULT '[]';
