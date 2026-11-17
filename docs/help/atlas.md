# Atlas

The unified graph, read-only, in five architecture bands (Infrastructure →
Cloud → Server code → Events → Client code) at deterministic positions.
"Server code" and "Client code" are recovered source, not machines. Past a
size threshold the initial view collapses per-band clusters that expand on
demand.

- **Large estates**: a band with more than 60 clusters opens as groups
  (repository, then ranges of cluster names), never more than 60 tiles at a
  time. Click a group, or its entry under **Collapsed groups**, to open it;
  **Open groups** folds it back. Edges between groups are combined into one
  line with a count.

- **Focus mode**: select a node and press **Enter** to re-root the view on it
  and its direct connections; **Esc** backs out one level; the breadcrumb
  shows the focus path.
- The **confidence overlay** colors facts by confidence; shapes encode kind
  (octagon = gap, diamond = channel) so color never carries alone.
- A **configuration read** (an environment variable read in source) is in
  the Server code band, in the cluster of the file that reads it. It is
  labelled with the key it reads. A read whose key is not written as a
  literal in the source is labelled "key not named in source".
- Every node and edge opens read-only evidence.
