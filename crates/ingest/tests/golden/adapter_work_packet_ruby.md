# Local work packet

Execution: unoffered. Capability requests confer no authority.

## Packet identity

    "work-packet-v1:bba01c364967e0a739fdeae0c5e61bd8f719f5d8b9d2204867fad6b9cfda49af"

## Body schema version

    1

## Lineage (outside reviewed body; additions do not change review revision)

    {
      "predecessor_ids": []
    }

## Review (independent metadata)

    {
      "revision": 0,
      "status": "draft",
      "note": null
    }

## Freshness (scoped observation)

    {
      "checked_at": null,
      "state": "unknown",
      "observed_snapshot_id": null,
      "checks": [],
      "limitations": [
        "No live-source assessment has been performed; producer input closure is incomplete"
      ]
    }

## 1. Identity

    {
      "generator_version": "adapter-work-packet@1",
      "template_version": "work-packet@1",
      "application": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "target application identity was not supplied"
        }
      },
      "recovered_basis": {
        "view": "recovered_graph",
        "snapshot_id": "context-v1:0000000000000000000000000000000000000000000000000000000000000001"
      },
      "sources": [
        {
          "registration_id": "src-ruby-fixture",
          "repo_key": "local/ruby-fixture",
          "revision": {
            "state": "supplied",
            "value": "0123456789abcdef0123456789abcdef01234567",
            "origin": {
              "kind": "evidence",
              "evidence_ids": [
                "fact:aba6f62a77dcbd2cd87206d7b684702fc0105be446b9df965ac671b891a3fd67",
                "fact:210537a31526c6d2c36013dc51ee08404ac4e6c4e7b82d8ed5f5d8392e91b66e",
                "fact:c648d47a8c31e6ad15d559e71100979ae0517e3778ba03958ec7c34a15e509fd"
              ]
            }
          }
        }
      ],
      "configuration": [],
      "input_closure": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "producer input/configuration closure is not established"
        }
      },
      "curated_context": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "H3: curated context is unavailable in this product slice"
        }
      }
    }

## 2. Intent (requests and claims)

    {
      "objective": {
        "state": "supplied",
        "value": "Make Ruby a covered language: write an adapter that claims the 3 target cell(s) below, with the golden cases proving each cell written first.",
        "origin": {
          "kind": "evidence",
          "evidence_ids": [
            "fact:aba6f62a77dcbd2cd87206d7b684702fc0105be446b9df965ac671b891a3fd67",
            "fact:210537a31526c6d2c36013dc51ee08404ac4e6c4e7b82d8ed5f5d8392e91b66e",
            "fact:c648d47a8c31e6ad15d559e71100979ae0517e3778ba03958ec7c34a15e509fd"
          ]
        }
      },
      "expected_outcome": {
        "state": "supplied",
        "value": "Each claimed cell is proved by golden cases the plugin gate passes and is declared supported in the plugin's coverage; a re-ingest reports the cell as covered.",
        "origin": {
          "kind": "evidence",
          "evidence_ids": [
            "fact:aba6f62a77dcbd2cd87206d7b684702fc0105be446b9df965ac671b891a3fd67",
            "fact:210537a31526c6d2c36013dc51ee08404ac4e6c4e7b82d8ed5f5d8392e91b66e",
            "fact:c648d47a8c31e6ad15d559e71100979ae0517e3778ba03958ec7c34a15e509fd"
          ]
        }
      },
      "current_behavior": {
        "state": "supplied",
        "value": "No compiled-in adapter or installed plugin declares coverage of these Ruby cells (adapter lookup: none), so Preflight reports them as an unsupported tool limitation.",
        "origin": {
          "kind": "evidence",
          "evidence_ids": [
            "fact:aba6f62a77dcbd2cd87206d7b684702fc0105be446b9df965ac671b891a3fd67",
            "fact:210537a31526c6d2c36013dc51ee08404ac4e6c4e7b82d8ed5f5d8392e91b66e",
            "fact:c648d47a8c31e6ad15d559e71100979ae0517e3778ba03958ec7c34a15e509fd"
          ]
        }
      },
      "intended_behavior": {
        "state": "supplied",
        "value": "Claim: ruby.core.calls: CoverageCell { language: \"Ruby\", framework: \"core\", capability: \"calls\", support: Supported }; ruby.core.imports: CoverageCell { language: \"Ruby\", framework: \"core\", capability: \"imports\", support: Supported }; ruby.core.types: CoverageCell { language: \"Ruby\", framework: \"core\", capability: \"types\", support: Supported }",
        "origin": {
          "kind": "evidence",
          "evidence_ids": [
            "fact:aba6f62a77dcbd2cd87206d7b684702fc0105be446b9df965ac671b891a3fd67",
            "fact:210537a31526c6d2c36013dc51ee08404ac4e6c4e7b82d8ed5f5d8392e91b66e",
            "fact:c648d47a8c31e6ad15d559e71100979ae0517e3778ba03958ec7c34a15e509fd"
          ]
        }
      },
      "scope": {
        "state": "supplied",
        "value": "Adapter contract: ADR-0017 WASM plugin (docs/adapters/AUTHORING_GUIDE.md Path A), because no compiled-in crate covers Ruby. Target cells: ruby.core.calls, ruby.core.imports, ruby.core.types.",
        "origin": {
          "kind": "evidence",
          "evidence_ids": [
            "fact:aba6f62a77dcbd2cd87206d7b684702fc0105be446b9df965ac671b891a3fd67",
            "fact:210537a31526c6d2c36013dc51ee08404ac4e6c4e7b82d8ed5f5d8392e91b66e",
            "fact:c648d47a8c31e6ad15d559e71100979ae0517e3778ba03958ec7c34a15e509fd"
          ]
        }
      },
      "non_goals": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "intent.non_goals was not supplied"
        }
      },
      "business_rationale": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "intent.business_rationale was not supplied"
        }
      }
    }

## 3. Evidence

    {
      "facts": [
        {
          "id": "fact:aba6f62a77dcbd2cd87206d7b684702fc0105be446b9df965ac671b891a3fd67",
          "reference": {
            "kind": "node",
            "id": "coverage-cell:ruby.core.calls"
          },
          "expected_digest": "node-v1:5d00823b71be0662c2a4ed370c870a8d63c2702791f0d6afb4035d65f7736051",
          "fact": {
            "kind": "node",
            "value": {
              "id": "coverage-cell:ruby.core.calls",
              "label": "CoverageCell",
              "props": {
                "adapter_lookup": "none",
                "capability": "calls",
                "framework": "core",
                "language": "Ruby",
                "note": "no installed adapter covers this language",
                "prov": {
                  "confidence_tier": "Confirmed",
                  "content_hash": "4415945b5f22c6cc16a2063177f55e2ed58d49e1ff6e28a79a91349e5ee9e823",
                  "evidence": [],
                  "extractor_id": "adapter-work-packet@1",
                  "tier": "Deterministic"
                },
                "support": "unsupported"
              }
            }
          },
          "provenance": {
            "tier": "Deterministic",
            "confidence_tier": "Confirmed",
            "evidence": [],
            "extractor_id": "adapter-work-packet@1",
            "content_hash": "4415945b5f22c6cc16a2063177f55e2ed58d49e1ff6e28a79a91349e5ee9e823"
          },
          "provenance_problem": null,
          "source_association": {
            "state": "missing",
            "omission": {
              "class": "missing_input",
              "seam": "interpretation",
              "detail": "generated from a file listing, not a captured source"
            }
          },
          "review_annotations": []
        },
        {
          "id": "fact:210537a31526c6d2c36013dc51ee08404ac4e6c4e7b82d8ed5f5d8392e91b66e",
          "reference": {
            "kind": "node",
            "id": "coverage-cell:ruby.core.imports"
          },
          "expected_digest": "node-v1:4d97ee4bbeeb7dbcf5bb3d5cc2907b0a715867e580551ae9f599cb590830840d",
          "fact": {
            "kind": "node",
            "value": {
              "id": "coverage-cell:ruby.core.imports",
              "label": "CoverageCell",
              "props": {
                "adapter_lookup": "none",
                "capability": "imports",
                "framework": "core",
                "language": "Ruby",
                "note": "no installed adapter covers this language",
                "prov": {
                  "confidence_tier": "Confirmed",
                  "content_hash": "06aa2097c7241c1557d1caac4164b241a8644b662d692cf5008148f4f5a5334b",
                  "evidence": [],
                  "extractor_id": "adapter-work-packet@1",
                  "tier": "Deterministic"
                },
                "support": "unsupported"
              }
            }
          },
          "provenance": {
            "tier": "Deterministic",
            "confidence_tier": "Confirmed",
            "evidence": [],
            "extractor_id": "adapter-work-packet@1",
            "content_hash": "06aa2097c7241c1557d1caac4164b241a8644b662d692cf5008148f4f5a5334b"
          },
          "provenance_problem": null,
          "source_association": {
            "state": "missing",
            "omission": {
              "class": "missing_input",
              "seam": "interpretation",
              "detail": "generated from a file listing, not a captured source"
            }
          },
          "review_annotations": []
        },
        {
          "id": "fact:c648d47a8c31e6ad15d559e71100979ae0517e3778ba03958ec7c34a15e509fd",
          "reference": {
            "kind": "node",
            "id": "coverage-cell:ruby.core.types"
          },
          "expected_digest": "node-v1:a46dc0adffbd8d848a3e80402809fa751596af84723187866f80a75c1fd8271f",
          "fact": {
            "kind": "node",
            "value": {
              "id": "coverage-cell:ruby.core.types",
              "label": "CoverageCell",
              "props": {
                "adapter_lookup": "none",
                "capability": "types",
                "framework": "core",
                "language": "Ruby",
                "note": "no installed adapter covers this language",
                "prov": {
                  "confidence_tier": "Confirmed",
                  "content_hash": "8ebba07c8e1a273904a684d9d4f3bebccd2a11f2991e3f858b623974953cc2b8",
                  "evidence": [],
                  "extractor_id": "adapter-work-packet@1",
                  "tier": "Deterministic"
                },
                "support": "unsupported"
              }
            }
          },
          "provenance": {
            "tier": "Deterministic",
            "confidence_tier": "Confirmed",
            "evidence": [],
            "extractor_id": "adapter-work-packet@1",
            "content_hash": "8ebba07c8e1a273904a684d9d4f3bebccd2a11f2991e3f858b623974953cc2b8"
          },
          "provenance_problem": null,
          "source_association": {
            "state": "missing",
            "omission": {
              "class": "missing_input",
              "seam": "interpretation",
              "detail": "generated from a file listing, not a captured source"
            }
          },
          "review_annotations": []
        },
        {
          "id": "fact:2b6433b63e8d4b9cd046413486855f40ae8c62fb22e52afb97901ec4d721b9ee",
          "reference": {
            "kind": "node",
            "id": "source-sample:app/controllers/users_controller.rb"
          },
          "expected_digest": "node-v1:207b894cd16c86c9af121a77e699c76751904c4f6122065dd42033777fd083a4",
          "fact": {
            "kind": "node",
            "value": {
              "id": "source-sample:app/controllers/users_controller.rb",
              "label": "SourceSample",
              "props": {
                "byte_len": 62,
                "language": "Ruby",
                "path": "app/controllers/users_controller.rb",
                "prov": {
                  "confidence_tier": "Confirmed",
                  "content_hash": "fd07cde708ff0213987f5ed4675fdb0ad2fea70051b0e608c0fd61c3a5826786",
                  "evidence": [
                    {
                      "byte_end": 62,
                      "byte_start": 0,
                      "commit_sha": "0123456789abcdef0123456789abcdef01234567",
                      "path": "app/controllers/users_controller.rb",
                      "repo": "local/ruby-fixture"
                    }
                  ],
                  "extractor_id": "adapter-work-packet@1",
                  "tier": "Deterministic"
                },
                "span": {
                  "end": 62,
                  "start": 0
                }
              }
            }
          },
          "provenance": {
            "tier": "Deterministic",
            "confidence_tier": "Confirmed",
            "evidence": [
              {
                "repo": "local/ruby-fixture",
                "path": "app/controllers/users_controller.rb",
                "byte_start": 0,
                "byte_end": 62,
                "commit_sha": "0123456789abcdef0123456789abcdef01234567"
              }
            ],
            "extractor_id": "adapter-work-packet@1",
            "content_hash": "fd07cde708ff0213987f5ed4675fdb0ad2fea70051b0e608c0fd61c3a5826786"
          },
          "provenance_problem": null,
          "source_association": {
            "state": "missing",
            "omission": {
              "class": "missing_input",
              "seam": "interpretation",
              "detail": "generated from a file listing, not a captured source"
            }
          },
          "review_annotations": []
        },
        {
          "id": "fact:3497f26ed283482bb8c8a43b155755b71d6996d99e6d77bc21de38bf63ea8caa",
          "reference": {
            "kind": "node",
            "id": "source-sample:app/models/order.rb"
          },
          "expected_digest": "node-v1:e44ff65ff8400f204f7e2a168c8032ab02739d270c7632688750e4c8f01e1cfd",
          "fact": {
            "kind": "node",
            "value": {
              "id": "source-sample:app/models/order.rb",
              "label": "SourceSample",
              "props": {
                "byte_len": 40,
                "language": "Ruby",
                "path": "app/models/order.rb",
                "prov": {
                  "confidence_tier": "Confirmed",
                  "content_hash": "1b86df3a07d502047c4ec28a2d0ce32a6f7faa4aa05464460f8a1ec5e9e249a7",
                  "evidence": [
                    {
                      "byte_end": 40,
                      "byte_start": 0,
                      "commit_sha": "0123456789abcdef0123456789abcdef01234567",
                      "path": "app/models/order.rb",
                      "repo": "local/ruby-fixture"
                    }
                  ],
                  "extractor_id": "adapter-work-packet@1",
                  "tier": "Deterministic"
                },
                "span": {
                  "end": 40,
                  "start": 0
                }
              }
            }
          },
          "provenance": {
            "tier": "Deterministic",
            "confidence_tier": "Confirmed",
            "evidence": [
              {
                "repo": "local/ruby-fixture",
                "path": "app/models/order.rb",
                "byte_start": 0,
                "byte_end": 40,
                "commit_sha": "0123456789abcdef0123456789abcdef01234567"
              }
            ],
            "extractor_id": "adapter-work-packet@1",
            "content_hash": "1b86df3a07d502047c4ec28a2d0ce32a6f7faa4aa05464460f8a1ec5e9e249a7"
          },
          "provenance_problem": null,
          "source_association": {
            "state": "missing",
            "omission": {
              "class": "missing_input",
              "seam": "interpretation",
              "detail": "generated from a file listing, not a captured source"
            }
          },
          "review_annotations": []
        },
        {
          "id": "fact:6d439db177918406e080f4148d15d40ad43f17777f0064a2aa2d084717d93b44",
          "reference": {
            "kind": "node",
            "id": "source-sample:app/models/user.rb"
          },
          "expected_digest": "node-v1:4d9d195d85966c11774c0014ee3184cc5c72d45e05e1056f233adff5e1f80c0f",
          "fact": {
            "kind": "node",
            "value": {
              "id": "source-sample:app/models/user.rb",
              "label": "SourceSample",
              "props": {
                "byte_len": 56,
                "language": "Ruby",
                "path": "app/models/user.rb",
                "prov": {
                  "confidence_tier": "Confirmed",
                  "content_hash": "f0e64966d482cb147143952fc02c898b6d20a8faeb8b3fce18c4ff8e9a03c9cf",
                  "evidence": [
                    {
                      "byte_end": 56,
                      "byte_start": 0,
                      "commit_sha": "0123456789abcdef0123456789abcdef01234567",
                      "path": "app/models/user.rb",
                      "repo": "local/ruby-fixture"
                    }
                  ],
                  "extractor_id": "adapter-work-packet@1",
                  "tier": "Deterministic"
                },
                "span": {
                  "end": 56,
                  "start": 0
                }
              }
            }
          },
          "provenance": {
            "tier": "Deterministic",
            "confidence_tier": "Confirmed",
            "evidence": [
              {
                "repo": "local/ruby-fixture",
                "path": "app/models/user.rb",
                "byte_start": 0,
                "byte_end": 56,
                "commit_sha": "0123456789abcdef0123456789abcdef01234567"
              }
            ],
            "extractor_id": "adapter-work-packet@1",
            "content_hash": "f0e64966d482cb147143952fc02c898b6d20a8faeb8b3fce18c4ff8e9a03c9cf"
          },
          "provenance_problem": null,
          "source_association": {
            "state": "missing",
            "omission": {
              "class": "missing_input",
              "seam": "interpretation",
              "detail": "generated from a file listing, not a captured source"
            }
          },
          "review_annotations": []
        },
        {
          "id": "fact:86eb3d588d584f60be77e10e72304e5edab310f68731c74ee52097cc92199dd5",
          "reference": {
            "kind": "node",
            "id": "source-sample:lib/util.rb"
          },
          "expected_digest": "node-v1:8a1b7dc73f00dbf49eb3d7188fb7917302ee830cd1350c6a3102807b104af77d",
          "fact": {
            "kind": "node",
            "value": {
              "id": "source-sample:lib/util.rb",
              "label": "SourceSample",
              "props": {
                "byte_len": 37,
                "language": "Ruby",
                "path": "lib/util.rb",
                "prov": {
                  "confidence_tier": "Confirmed",
                  "content_hash": "6abe34e069429b6a9d0bc51a12b47ad99377d29cfffceeaf77398b390476bb38",
                  "evidence": [
                    {
                      "byte_end": 37,
                      "byte_start": 0,
                      "commit_sha": "0123456789abcdef0123456789abcdef01234567",
                      "path": "lib/util.rb",
                      "repo": "local/ruby-fixture"
                    }
                  ],
                  "extractor_id": "adapter-work-packet@1",
                  "tier": "Deterministic"
                },
                "span": {
                  "end": 37,
                  "start": 0
                }
              }
            }
          },
          "provenance": {
            "tier": "Deterministic",
            "confidence_tier": "Confirmed",
            "evidence": [
              {
                "repo": "local/ruby-fixture",
                "path": "lib/util.rb",
                "byte_start": 0,
                "byte_end": 37,
                "commit_sha": "0123456789abcdef0123456789abcdef01234567"
              }
            ],
            "extractor_id": "adapter-work-packet@1",
            "content_hash": "6abe34e069429b6a9d0bc51a12b47ad99377d29cfffceeaf77398b390476bb38"
          },
          "provenance_problem": null,
          "source_association": {
            "state": "missing",
            "omission": {
              "class": "missing_input",
              "seam": "interpretation",
              "detail": "generated from a file listing, not a captured source"
            }
          },
          "review_annotations": []
        }
      ],
      "artifacts": [],
      "source_excerpts": [],
      "interpretations": [],
      "limitations": []
    }

## 4. Work (proposals)

    {
      "affected_components": {
        "state": "supplied",
        "value": [
          {
            "repo_key": "local/ruby-fixture",
            "path": "app/controllers/users_controller.rb",
            "fact_references": [
              {
                "kind": "node",
                "id": "source-sample:app/controllers/users_controller.rb"
              }
            ]
          },
          {
            "repo_key": "local/ruby-fixture",
            "path": "app/models/order.rb",
            "fact_references": [
              {
                "kind": "node",
                "id": "source-sample:app/models/order.rb"
              }
            ]
          },
          {
            "repo_key": "local/ruby-fixture",
            "path": "app/models/user.rb",
            "fact_references": [
              {
                "kind": "node",
                "id": "source-sample:app/models/user.rb"
              }
            ]
          },
          {
            "repo_key": "local/ruby-fixture",
            "path": "lib/util.rb",
            "fact_references": [
              {
                "kind": "node",
                "id": "source-sample:lib/util.rb"
              }
            ]
          }
        ],
        "origin": {
          "kind": "evidence",
          "evidence_ids": [
            "fact:2b6433b63e8d4b9cd046413486855f40ae8c62fb22e52afb97901ec4d721b9ee",
            "fact:3497f26ed283482bb8c8a43b155755b71d6996d99e6d77bc21de38bf63ea8caa",
            "fact:6d439db177918406e080f4148d15d40ad43f17777f0064a2aa2d084717d93b44",
            "fact:86eb3d588d584f60be77e10e72304e5edab310f68731c74ee52097cc92199dd5"
          ]
        }
      },
      "proposed_approach": {
        "state": "supplied",
        "value": [
          "Write the golden corpus first, {plugin-id}.golden.json, with cases from representative sources: golden case(s) proving ruby.core.calls, golden case(s) proving ruby.core.imports, golden case(s) proving ruby.core.types.",
          "Declare each target cell in the corpus `coverage` as supported, naming the golden cases that prove it.",
          "Build {plugin-id}.wasm against the ADR-0017 SPI until every golden case passes.",
          "Copy the .wasm and .golden.json into .cartograph/adapters/, gate the plugin in Settings, then re-ingest and confirm Preflight reports each target cell as covered."
        ],
        "origin": {
          "kind": "evidence",
          "evidence_ids": [
            "fact:aba6f62a77dcbd2cd87206d7b684702fc0105be446b9df965ac671b891a3fd67",
            "fact:210537a31526c6d2c36013dc51ee08404ac4e6c4e7b82d8ed5f5d8392e91b66e",
            "fact:c648d47a8c31e6ad15d559e71100979ae0517e3778ba03958ec7c34a15e509fd"
          ]
        }
      },
      "dependencies": {
        "state": "supplied",
        "value": [
          {
            "description": "ADR-0017 runtime WASM adapter plugin host",
            "evidence_ids": []
          },
          {
            "description": "docs/adapters/AUTHORING_GUIDE.md",
            "evidence_ids": []
          }
        ],
        "origin": {
          "kind": "evidence",
          "evidence_ids": [
            "fact:aba6f62a77dcbd2cd87206d7b684702fc0105be446b9df965ac671b891a3fd67",
            "fact:210537a31526c6d2c36013dc51ee08404ac4e6c4e7b82d8ed5f5d8392e91b66e",
            "fact:c648d47a8c31e6ad15d559e71100979ae0517e3778ba03958ec7c34a15e509fd"
          ]
        }
      },
      "open_decisions": {
        "state": "supplied",
        "value": [
          "The plugin id ({plugin-id} above)."
        ],
        "origin": {
          "kind": "evidence",
          "evidence_ids": [
            "fact:aba6f62a77dcbd2cd87206d7b684702fc0105be446b9df965ac671b891a3fd67",
            "fact:210537a31526c6d2c36013dc51ee08404ac4e6c4e7b82d8ed5f5d8392e91b66e",
            "fact:c648d47a8c31e6ad15d559e71100979ae0517e3778ba03958ec7c34a15e509fd"
          ]
        }
      }
    }

## 5. Acceptance (proposed verification and distinct observations)

    {
      "positive_cases": {
        "state": "supplied",
        "value": [
          {
            "id": "fixture-ruby.core.calls",
            "description": "Cell ruby.core.calls is proved by its golden cases.",
            "steps": [
              "Add golden cases with representative Ruby sources whose expected output proves ruby.core.calls.",
              "Declare ruby.core.calls in the corpus `coverage`, naming those cases (at least one asserts nodes or edges).",
              "Gate the plugin; every golden case and the coverage-declarations check must pass."
            ],
            "expected_evidence": [
              "the plugin gate passes with ruby.core.calls declared"
            ],
            "evidence_ids": [
              "fact:aba6f62a77dcbd2cd87206d7b684702fc0105be446b9df965ac671b891a3fd67"
            ]
          },
          {
            "id": "fixture-ruby.core.imports",
            "description": "Cell ruby.core.imports is proved by its golden cases.",
            "steps": [
              "Add golden cases with representative Ruby sources whose expected output proves ruby.core.imports.",
              "Declare ruby.core.imports in the corpus `coverage`, naming those cases (at least one asserts nodes or edges).",
              "Gate the plugin; every golden case and the coverage-declarations check must pass."
            ],
            "expected_evidence": [
              "the plugin gate passes with ruby.core.imports declared"
            ],
            "evidence_ids": [
              "fact:210537a31526c6d2c36013dc51ee08404ac4e6c4e7b82d8ed5f5d8392e91b66e"
            ]
          },
          {
            "id": "fixture-ruby.core.types",
            "description": "Cell ruby.core.types is proved by its golden cases.",
            "steps": [
              "Add golden cases with representative Ruby sources whose expected output proves ruby.core.types.",
              "Declare ruby.core.types in the corpus `coverage`, naming those cases (at least one asserts nodes or edges).",
              "Gate the plugin; every golden case and the coverage-declarations check must pass."
            ],
            "expected_evidence": [
              "the plugin gate passes with ruby.core.types declared"
            ],
            "evidence_ids": [
              "fact:c648d47a8c31e6ad15d559e71100979ae0517e3778ba03958ec7c34a15e509fd"
            ]
          }
        ],
        "origin": {
          "kind": "evidence",
          "evidence_ids": [
            "fact:aba6f62a77dcbd2cd87206d7b684702fc0105be446b9df965ac671b891a3fd67",
            "fact:210537a31526c6d2c36013dc51ee08404ac4e6c4e7b82d8ed5f5d8392e91b66e",
            "fact:c648d47a8c31e6ad15d559e71100979ae0517e3778ba03958ec7c34a15e509fd"
          ]
        }
      },
      "negative_cases": {
        "state": "supplied",
        "value": [
          {
            "id": "claim-without-fixture",
            "description": "A claimed cell naming no golden case is rejected.",
            "steps": [
              "Gate the plugin with a cell declared in `coverage` and no golden case named for it."
            ],
            "expected_evidence": [
              "the coverage-declarations check fails on that cell"
            ],
            "evidence_ids": [
              "fact:aba6f62a77dcbd2cd87206d7b684702fc0105be446b9df965ac671b891a3fd67",
              "fact:210537a31526c6d2c36013dc51ee08404ac4e6c4e7b82d8ed5f5d8392e91b66e",
              "fact:c648d47a8c31e6ad15d559e71100979ae0517e3778ba03958ec7c34a15e509fd"
            ]
          }
        ],
        "origin": {
          "kind": "evidence",
          "evidence_ids": [
            "fact:aba6f62a77dcbd2cd87206d7b684702fc0105be446b9df965ac671b891a3fd67",
            "fact:210537a31526c6d2c36013dc51ee08404ac4e6c4e7b82d8ed5f5d8392e91b66e",
            "fact:c648d47a8c31e6ad15d559e71100979ae0517e3778ba03958ec7c34a15e509fd"
          ]
        }
      },
      "proposed_verification": {
        "state": "supplied",
        "value": [
          {
            "id": "matrix-updated",
            "description": "The plugin's coverage declares every claimed cell.",
            "steps": [
              "Compare the corpus `coverage` against the target cells.",
              "Re-run Preflight on the repo."
            ],
            "expected_evidence": [
              "each target cell is declared supported in the plugin's coverage",
              "adapter lookup reports each target cell as covered"
            ],
            "evidence_ids": [
              "fact:aba6f62a77dcbd2cd87206d7b684702fc0105be446b9df965ac671b891a3fd67",
              "fact:210537a31526c6d2c36013dc51ee08404ac4e6c4e7b82d8ed5f5d8392e91b66e",
              "fact:c648d47a8c31e6ad15d559e71100979ae0517e3778ba03958ec7c34a15e509fd"
            ]
          }
        ],
        "origin": {
          "kind": "evidence",
          "evidence_ids": [
            "fact:aba6f62a77dcbd2cd87206d7b684702fc0105be446b9df965ac671b891a3fd67",
            "fact:210537a31526c6d2c36013dc51ee08404ac4e6c4e7b82d8ed5f5d8392e91b66e",
            "fact:c648d47a8c31e6ad15d559e71100979ae0517e3778ba03958ec7c34a15e509fd"
          ]
        }
      },
      "discovered_tests": [],
      "structural_test_associations": [],
      "execution_evidence": [],
      "coverage_evidence": [],
      "assertion_support": []
    }

## 6. Handoff (requests, never grants)

    {
      "requested_capabilities": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "handoff.requested_capabilities was not supplied"
        }
      },
      "checkout_boundaries": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "handoff.checkout_boundaries was not supplied"
        }
      },
      "tool_and_egress_requests": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "handoff.tool_and_egress_requests was not supplied"
        }
      },
      "budgets": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "handoff.budgets was not supplied"
        }
      },
      "proposed_role": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "handoff.proposed_role was not supplied"
        }
      },
      "authorization_references": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "handoff.authorization_references was not supplied"
        }
      }
    }

## 7. Return (no worker completion claimed)

    {
      "implementation_revision": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "return.implementation_revision was not supplied"
        }
      },
      "pull_request": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "return.pull_request was not supplied"
        }
      },
      "execution_receipts": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "return.execution_receipts was not supplied"
        }
      },
      "source_configuration_changes": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "return.source_configuration_changes was not supplied"
        }
      },
      "reingest_comparison": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "return.reingest_comparison was not supplied"
        }
      },
      "residual_gaps": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "return.residual_gaps was not supplied"
        }
      }
    }
