# Agent Chats

The **Agent pool** at the top lists the agent work recorded as jobs: what is
queued or running, counted by kind (specialist investigations, topic-expert
questions, section drafting and gap escalation), and the last eight finished
agent jobs. It also shows each configured provider and model with its catalog
availability, and how many staged proposals in the loaded page wait for a
decision. It reads those records only: a job record does not prove a worker is
live, a configured provider is not proof a model answers, and agent output is
never promoted over T0/T1 facts. **Check local server** asks the local model
server on this machine whether it is running and which models are missing; it
calls no cloud provider. Use **Open Jobs** to cancel or retry work and
**Open curation** to decide proposals.

Under each topic-expert question and section draft, the pool shows how many
model calls it made and the input and output tokens the provider reported, with
a total over the listed work. A call whose provider reported no count is shown
as unreported, never estimated, and none of this is verified billing.
Specialist investigations keep their usage with each saved task.

The pool's **Controls** pause new agent work or limit how many local and cloud
agent jobs run at once. They apply when a specialist investigation,
topic-expert question, section draft or gap escalation starts: while the pool
is paused or a mode is full, the new job waits as queued, in the order it was
asked, and starts when a place frees or the pool is resumed. Cancel a waiting
job from Jobs. Work already running continues. A waiting investigation's time
limits start when it starts. At most two investigations are open at once,
whatever the limits say. An investigation still waiting when the app closes
is ended as interrupted the next time the app starts; start it again if you
still need it.

Review saved specialist conversations and open their findings. The list uses
the same saved investigation history as Investigations, including its search,
active filter, and **Load older saved tasks** continuation. Each docked task
shows its saved question, producer identity, and typed result reference when
those records are available. Older tasks without a persisted conversation
remain identifiable and can still be opened in Investigations.

Choose **Open chat** on a saved-result notice to dock that task's validated
persisted conversation in Agent Chats. Choose **Open findings** to open the
matching task in Investigations. Viewing a transcript keeps its saved-result
notice unread; only showing the matching findings consumes that notice. These
actions do not start work or change the composer or an uncertain start request.
The notice is not a message-read indicator, and this surface does not provide
freeform assistant messages or streaming.

Select history entries to dock up to four real saved task chats. Reopening an
existing chat focuses and refreshes it without changing its docking age. A fifth
successfully loaded chat replaces the oldest dock; failed and legacy reads do
not replace a pane. Close removes only the pane, so history can reopen it.

For a supported saved task with a validated result, choose **Follow-up task**
to prepare a new child investigation from that exact task. The dialog shows its
saved scope, specialist version, provider mode and producer attribution; the
current configured provider for that mode is used. Completion roles require
fresh recorded inventory choices. The follow-up is a separate task and saved
chat linked to the parent, not a message appended to the parent transcript.
Only a confirmed child opens in Agent Chats; an uncertain start retains the
same request and nonce for retry. Preparation or admission failures never
substitute another parent or specialist. This increment does not provide
free-form replies or streaming.

Panes retain last-observed snapshots, including pending chats; they do not imply
live updates. Use **Focus / refresh** to reread a pane. Layout adjusts to one
through four panes and is session-only. These are individual task chats, not
merged multi-task conversation threads.

Choose **Conversation turns** on a pane to browse saved tasks in its validated
conversation, newest first. Each row keeps its specialist version, status and
explicit parent link; opening one uses the same bounded dock reader. An off-page
selected turn remains pinned. **Load older turns** continues the existing page;
**Refresh conversation turns** discovers new saved children. Errors keep already
loaded turns visible. These are linked task chats with separate findings and
citations, not one appended transcript. Close the navigator to end that view.

Completed saved chats show their validated typed findings, claim kinds,
limitations and citation metadata directly in the pane. These retain T3 /
InferredWeak attribution and do not change confirmed facts. **Inspect citation**
refreshes that exact task before reading its retained historical evidence;
switching or closing cancels presentation of late evidence. No current checkout
source is substituted. Pending or unavailable chats have no admitted answer.

Choose **Reply here** to use the same supported follow-up form inside a saved
pane. Send creates a separate child task in the same conversation; its actual
saved answer opens using the normal four-pane rule. The selected parent result
is supplied as bounded T3 history, not all earlier questions or answers. Closing
before submission cancels this input; submitted or uncertain work retains its
existing durable request and retry identity. The summon shortcut opens the
standalone dialog instead.

## Loaded conversation branches

Loaded saved exchanges are grouped by their persisted conversation ID. Each
exchange keeps its own question, producer, findings, citations and reply form.
Parent links show saved ancestry; sibling tasks remain separate forks. An
unloaded parent can be opened explicitly using the same four-slot reader,
which may evict the oldest loaded exchange. No related details load automatically.
These last-observed groups are not complete history: use **Conversation turns**
for bounded saved-turn pages. Replies still receive only the selected parent
result as context, not every visible exchange or full ancestral dialogue.

## Focused task progress

The focused pane reconciles its saved status and committed findings from the
existing task refresh. Its Activity section shows actual recorded host events
only after the matching task and transcript settle. During refresh or errors,
retained content remains a last-observed snapshot. Other panes update when
focused or refreshed. This is durable execution progress, not generated tokens
or a streaming assistant reply.

## Saved answer search

Saved history search includes accepted final narrative text, finding titles and
statements alongside questions, saved specialist display names and specialist IDs. **Answer match** marks an answer-only hit. No
answer or source excerpt is returned in history; open the task for its validated
final message, findings and citations. Rejected responses and raw source are not searched.
Historical tasks without a narrative still match their existing saved fields.

## Saved result notice position

Choose **Saved result notice position** to place existing saved-result notices
in any corner. The choice is stored locally; when local storage is unavailable,
notices use the default top-right position.

Spec Writer @2 can return a saved, cited final message with its proposed design. Its text is provider-authored and admitted with the result, labeled T3 / InferredWeak. Insufficient evidence has no invented message. This is final output, not token streaming; follow-ups still receive only the bounded selected-parent result.

Gap Triager @2 can return a cited final next-step message grounded in recorded gaps and admitted evidence. Its saved proposed-design message is T3 / InferredWeak; insufficient evidence supplies no invented narrative. Gap Triager @1 history remains readable. Follow-ups retain the existing bounded selected-parent result, not the whole conversation.

**Name match** identifies a task found by its validated saved specialist display name, including a pending task without an answer. Names are not inferred from the current catalog or guessed aliases. Search remains literal and ASCII case-insensitive.

## Search open and saved chats

Use **Search open + saved chats** or **Cmd/Ctrl+F** in Agent Chats. Submit a literal search over saved specialist names or IDs, question titles and admitted answer text; this selects **all saved tasks**, even after an active-only filter. Editing alone makes no request. OPEN marks a currently docked task, HISTORY an undocked task. Open snapshots are last observed; older history uses the existing bounded pages. Picking a result docks it, evicting the oldest only after a fifth chat successfully loads. Escape closes the palette and returns focus without discarding an inline reply or pending request.

## Pane layout

Choose **Pane layout** 1, 2, 3 or 4 to arrange that many slots. Three slots offer a **Wide row** at the Top or Bottom; narrow windows stack slots in one column. Empty slots use **Choose saved chat** to open the existing search palette. Close panes first to select fewer slots than loaded chats: layout never silently closes or hides a chat. Opening more chats expands the arrangement up to the existing four-pane bound. These choices change presentation only and preserve replies and pending requests.

## Known unread saved results

The Agent Chats rail badge shows saved results the app has observed as unread in this session. A **?** beside the count, or **?** alone, means checks are incomplete; it is not a complete historical unread total. Dismissing a toast or opening its chat does not clear this existing inbox. Opening the exact validated findings does. The badge adds no background reads and does not change replies or pending requests.

## Saved pane attribution

Each pane header shows its saved specialist name and version, with its saved requested provider/model and Local or Cloud mode. These labels belong to that pane’s validated snapshot, not the current catalog or another selected task. A requested model is not a separately observed response model, and Local mode alone does not guarantee zero egress. The existing last-observed caveat still applies.

## Message evidence metadata

Specialist result references and saved final messages show T3 / InferredWeak, even when they cite confirmed facts. Counts distinguish unique source citations from graph citations; they do not mean source snippets were loaded. Result references count their findings’ citations, while final messages count only their supporting findings. Unavailable metadata is not a zero count. User questions are not attributed to the specialist, and existing citation inspection still uses the exact saved task.

## Reading a pane

Each open pane has its own **Saved messages** reading region. Focus it to scroll with the keyboard, or scroll it with the pointer; long output remains available without scrolling another pane. The pane header and reply stay outside the reading region. Your questions align right, specialist output left; citation inspection still uses the owning saved task. Layout and scrolling do not discard a reply or pending request.

Saved panes show provider-reported input, output and cache-read tokens **so far** for that task snapshot. Exact zero is zero, **Unknown** means no known report, and **Unavailable** means the optional projection is absent. These reports may be incomplete and are not a price or a conversation-wide total. Failed refresh keeps the last-observed pane and reports current progress unavailable.

**Check local Ollama** makes one explicit metadata request to `http://127.0.0.1:11434/`. Success means a valid bounded 2xx metadata response, not that a model is installed or ready; cloud providers are not checked. Network attempts use the existing 10-second timeout. Could-not-verify includes timeouts, refused connections and invalid responses, not proof a process is down. An existing check owns its slot until it finishes, even after you leave the view; retry explicitly if it is busy. Browser preview cannot check the local API. No task is started or provider setting changed.
