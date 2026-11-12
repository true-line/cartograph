;; Test fixture only: a `cartograph:adapter` component in text form, so no
;; wasm toolchain is needed. The host compiles it with `PluginHost::load`.
;;
;; `extract-source` picks its behaviour from the first byte of the source,
;; which makes one plugin succeed on some files of a tree and fail on others
;; (#1810, AC-0658):
;;
;;   M  one node whose props carry a `prov` field a plugin may not write
;;      (the host answers `MalformedFact`)
;;   E  `Err(extract-error)`            (`GuestError`)
;;   T  `unreachable`                   (`Trap`)
;;   F  never returns                   (`FuelExhausted` or `DeadlineExceeded`,
;;                                       whichever bound is reached first)
;;   G  grows memory by 256 MiB         (`MemoryLimitExceeded`)
;;   B  grows a table by 20000 elements (`TableLimitExceeded`)
;;   S  recurses without end            (`StackOverflow`)
;;   anything else: one `TestNode` with id `{repo}:{path}` and props `{}`.
(component
  (core module $guest
    (memory (export "memory") 1)
    (table $grow 1 funcref)
    ;; Bytes 0..4096 are static: the return area at 8, one node record at
    ;; 64, the strings below, and the node id built at 1024. The host's
    ;; arguments are allocated from 4096 up.
    (global $bump (mut i32) (i32.const 4096))
    (data (i32.const 128) "TestNode")
    (data (i32.const 144) "{}")
    (data (i32.const 160) "{\"prov\":{\"method\":\"guess\"}}")
    (data (i32.const 200) "refused by guest")

    ;; A bump allocator: the host only allocates the call arguments.
    (func (export "cabi_realloc")
      (param $old i32) (param $old_size i32) (param $align i32) (param $new_size i32)
      (result i32)
      (local $ptr i32) (local $end i32) (local $have i32)
      (local.set $ptr
        (i32.and
          (i32.add (global.get $bump) (i32.sub (local.get $align) (i32.const 1)))
          (i32.xor (i32.sub (local.get $align) (i32.const 1)) (i32.const -1))))
      (local.set $end (i32.add (local.get $ptr) (local.get $new_size)))
      (global.set $bump (local.get $end))
      (local.set $have (i32.mul (memory.size) (i32.const 65536)))
      (if (i32.gt_u (local.get $end) (local.get $have))
        (then
          (if
            (i32.eq
              (memory.grow
                (i32.add
                  (i32.shr_u (i32.sub (local.get $end) (local.get $have)) (i32.const 16))
                  (i32.const 1)))
              (i32.const -1))
            (then unreachable))))
      (local.get $ptr))

    (func $recurse (param i32) (result i32)
      (i32.add (call $recurse (local.get 0)) (i32.const 1)))

    (func (export "extract")
      (param $source i32) (param $source_len i32)
      (param $path i32) (param $path_len i32)
      (param $repo i32) (param $repo_len i32)
      (param $commit i32) (param $commit_len i32)
      (result i32)
      (local $kind i32)
      (if (local.get $source_len)
        (then (local.set $kind (i32.load8_u (local.get $source)))))
      ;; T
      (if (i32.eq (local.get $kind) (i32.const 84)) (then unreachable))
      ;; F
      (if (i32.eq (local.get $kind) (i32.const 70)) (then (loop $spin (br $spin))))
      ;; G
      (if (i32.eq (local.get $kind) (i32.const 71))
        (then (drop (memory.grow (i32.const 4096)))))
      ;; B
      (if (i32.eq (local.get $kind) (i32.const 66))
        (then (drop (table.grow $grow (ref.null func) (i32.const 20000)))))
      ;; S
      (if (i32.eq (local.get $kind) (i32.const 83))
        (then (drop (call $recurse (i32.const 0)))))
      ;; E: result discriminant 1, then the message string.
      (if (i32.eq (local.get $kind) (i32.const 69))
        (then
          (i32.store8 (i32.const 8) (i32.const 1))
          (i32.store (i32.const 12) (i32.const 200))
          (i32.store (i32.const 16) (i32.const 16))
          (return (i32.const 8))))
      ;; The node id `{repo}:{path}`, built at 1024.
      (memory.copy (i32.const 1024) (local.get $repo) (local.get $repo_len))
      (i32.store8 (i32.add (i32.const 1024) (local.get $repo_len)) (i32.const 58))
      (memory.copy
        (i32.add (i32.const 1025) (local.get $repo_len))
        (local.get $path)
        (local.get $path_len))
      ;; The node record: id, label, props-json.
      (i32.store (i32.const 64) (i32.const 1024))
      (i32.store (i32.const 68)
        (i32.add (i32.add (local.get $repo_len) (local.get $path_len)) (i32.const 1)))
      (i32.store (i32.const 72) (i32.const 128))
      (i32.store (i32.const 76) (i32.const 8))
      (if (i32.eq (local.get $kind) (i32.const 77))
        (then
          ;; M
          (i32.store (i32.const 80) (i32.const 160))
          (i32.store (i32.const 84) (i32.const 27)))
        (else
          (i32.store (i32.const 80) (i32.const 144))
          (i32.store (i32.const 84) (i32.const 2))))
      ;; Result discriminant 0, then one node and no edge.
      (i32.store8 (i32.const 8) (i32.const 0))
      (i32.store (i32.const 12) (i32.const 64))
      (i32.store (i32.const 16) (i32.const 1))
      (i32.store (i32.const 20) (i32.const 0))
      (i32.store (i32.const 24) (i32.const 0))
      (i32.const 8)))

  (core instance $instance (instantiate $guest))
  (alias core export $instance "memory" (core memory $memory))
  (alias core export $instance "cabi_realloc" (core func $realloc))
  (alias core export $instance "extract" (core func $extract-core))

  (type $source-id (record (field "repo" string) (field "commit" string)))
  (type $node
    (record (field "id" string) (field "label" string) (field "props-json" string)))
  (type $edge
    (record
      (field "src" string) (field "dst" string) (field "label" string)
      (field "props-json" string)))
  (type $nodes (list $node))
  (type $edges (list $edge))
  (type $extraction (record (field "nodes" $nodes) (field "edges" $edges)))
  (type $extract-error (record (field "message" string)))
  (type $bytes (list u8))
  (type $outcome (result $extraction (error $extract-error)))
  (type $extract-source
    (func
      (param "source" $bytes) (param "path" string) (param "id" $source-id)
      (result $outcome)))
  (func $extract (type $extract-source)
    (canon lift (core func $extract-core) (memory $memory) (realloc $realloc)))

  ;; An exported function may only use named types, so the interface is a
  ;; nested component that imports the types and the function and exports
  ;; them under the names the WIT gives them.
  (component $interface
    (type $source-id-shape (record (field "repo" string) (field "commit" string)))
    (import "import-type-source-id" (type $source-id (eq $source-id-shape)))
    (type $node-shape
      (record (field "id" string) (field "label" string) (field "props-json" string)))
    (import "import-type-node" (type $node (eq $node-shape)))
    (type $edge-shape
      (record
        (field "src" string) (field "dst" string) (field "label" string)
        (field "props-json" string)))
    (import "import-type-edge" (type $edge (eq $edge-shape)))
    (type $nodes (list $node))
    (type $edges (list $edge))
    (type $extraction-shape (record (field "nodes" $nodes) (field "edges" $edges)))
    (import "import-type-extraction" (type $extraction (eq $extraction-shape)))
    (type $extract-error-shape (record (field "message" string)))
    (import "import-type-extract-error" (type $extract-error (eq $extract-error-shape)))
    (type $bytes (list u8))
    (type $outcome (result $extraction (error $extract-error)))
    (type $extract-source
      (func
        (param "source" $bytes) (param "path" string) (param "id" $source-id)
        (result $outcome)))
    (import "import-func-extract-source" (func $extract (type $extract-source)))
    (export $source-id-named "source-id" (type $source-id))
    (export $node-named "node" (type $node))
    (export $edge-named "edge" (type $edge))
    ;; `extraction` is exported as a record over the exported `node` and
    ;; `edge`, so every type it reaches is named in this instance.
    (type $nodes-named (list $node-named))
    (type $edges-named (list $edge-named))
    (type $extraction-shape-named
      (record (field "nodes" $nodes-named) (field "edges" $edges-named)))
    (export $extraction-named "extraction" (type $extraction)
      (type (eq $extraction-shape-named)))
    (export $extract-error-named "extract-error" (type $extract-error))
    (type $outcome-named (result $extraction-named (error $extract-error-named)))
    (type $extract-source-named
      (func
        (param "source" $bytes) (param "path" string) (param "id" $source-id-named)
        (result $outcome-named)))
    (export "extract-source" (func $extract) (func (type $extract-source-named))))

  (instance $extract-instance
    (instantiate $interface
      (with "import-func-extract-source" (func $extract))
      (with "import-type-source-id" (type $source-id))
      (with "import-type-node" (type $node))
      (with "import-type-edge" (type $edge))
      (with "import-type-extraction" (type $extraction))
      (with "import-type-extract-error" (type $extract-error))))
  (export "cartograph:adapter/extract@0.1.0" (instance $extract-instance)))
