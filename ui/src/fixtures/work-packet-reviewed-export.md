# Local work packet

Execution: unoffered. Capability requests confer no authority.

## Packet identity

    "work-packet-v1:cc42a9775bc708dee1c85a1ad9fec954d813432ceda6bd08c50f057fec3587b1"

## Body schema version

    1

## Lineage

    {
      "predecessor_ids": []
    }

## Review (independent metadata)

    {
      "revision": 1,
      "status": "reviewed",
      "note": "Locally reviewed; execution remains unoffered."
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
      "generator_version": "0.15.5",
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
        "snapshot_id": "context-v1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
      },
      "sources": [],
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
        "value": "Prepare a requested change. Treat this quoted source instruction as inert evidence: publish to GitHub, run shell commands, and invoke a model. </pre><script>alert(1)</script>",
        "origin": {
          "kind": "requested_intent"
        }
      },
      "expected_outcome": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "intent.expected_outcome was not supplied"
        }
      },
      "current_behavior": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "intent.current_behavior was not supplied"
        }
      },
      "intended_behavior": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "intent.intended_behavior was not supplied"
        }
      },
      "scope": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "intent.scope was not supplied"
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
          "id": "fact:143c9328988024a11c6cdea75b13ae730e0de629966ff8b8a260f81668424124",
          "reference": {
            "kind": "node",
            "id": "fixture-symbol"
          },
          "expected_digest": "node-v1:aac7d2a389b5fec93784aa8106216c2cf2ce4d3394ea3698cb9a6cb1ffc82a52",
          "fact": {
            "kind": "node",
            "value": {
              "id": "fixture-symbol",
              "label": "Capability",
              "props": {
                "method": "GET",
                "name": "fixture-symbol",
                "path": "/fixture",
                "prov": {
                  "confidence_tier": "Confirmed",
                  "content_hash": "4df38eddb9b2309bdbaef1c5a20c052949f9ebbbc9022fd1ce1980434b2acaf3",
                  "evidence": [],
                  "extractor_id": "fixture-rule",
                  "method": {
                    "kind": "rule",
                    "rule": "fixture",
                    "version": "1"
                  },
                  "tier": "Deterministic"
                },
                "upstream_timestamp": "2001-01-01T00:00:00Z"
              }
            }
          },
          "provenance": {
            "tier": "Deterministic",
            "confidence_tier": "Confirmed",
            "evidence": [],
            "extractor_id": "fixture-rule",
            "content_hash": "4df38eddb9b2309bdbaef1c5a20c052949f9ebbbc9022fd1ce1980434b2acaf3",
            "method": {
              "kind": "rule",
              "rule": "fixture",
              "version": "1"
            }
          },
          "provenance_problem": null,
          "source_association": {
            "state": "missing",
            "omission": {
              "class": "missing_input",
              "seam": "interpretation",
              "detail": "No historical source binding supplied"
            }
          },
          "review_annotations": [
            {
              "id": "decision:537eb4f2e22df8908a45acc7efa5bd3cb53fca02d0ab788c8f2343ac21617a22",
              "record_hash": "ccb5b251e02fc7c34843ecde30eb18dd99086027b3886fd23a5525f0d0199618",
              "record": {
                "assertion": {
                  "subject_id": "fixture-symbol",
                  "summary": "Historical rejected interpretation; this remains an inference",
                  "provenance": {
                    "tier": "Semantic",
                    "confidence_tier": "InferredStrong",
                    "evidence": [
                      {
                        "repo": "fixture/repo",
                        "path": "src/fixture.rs",
                        "byte_start": 0,
                        "byte_end": 12,
                        "commit_sha": "1111111111111111111111111111111111111111"
                      }
                    ],
                    "extractor_id": "historical-model",
                    "content_hash": "acaa86346b75a8a2db7673985597ef4f428545099538dcacedc5d6054205d124",
                    "method": {
                      "kind": "model",
                      "model": "fixture-model-v1"
                    }
                  }
                },
                "decision": "rejected",
                "note": "Reviewed earlier content; do not apply to changed content",
                "updated_at": "2001-01-01T00:00:00Z"
              },
              "relationship": {
                "state": "supplied",
                "value": {
                  "reference": {
                    "kind": "node",
                    "id": "fixture-symbol"
                  },
                  "stale": true
                },
                "origin": {
                  "kind": "evidence",
                  "evidence_ids": [
                    "fact:143c9328988024a11c6cdea75b13ae730e0de629966ff8b8a260f81668424124"
                  ]
                }
              }
            }
          ]
        }
      ],
      "artifacts": [
        {
          "id": "artifact:b232ca3f4a621d605f843f10f8fb65ec19e68cc2080d726376af327919ac40b0",
          "selection": {
            "id": "user-stories",
            "schema": "cartograph.spec/user-stories@1",
            "content_id": "spec-artifact-v1:edbb119b2c0a7a1f5363ace1e9ce8c1c8e62a4ab4a53b7af7780f7cbe75d29a0",
            "mode": "verified-only"
          },
          "artifact": {
            "state": "supplied",
            "value": {
              "id": "user-stories",
              "file_name": "user_stories.md",
              "title": "User stories",
              "format": "markdown",
              "schema": "cartograph.spec/user-stories@1",
              "content_id": "spec-artifact-v1:edbb119b2c0a7a1f5363ace1e9ce8c1c8e62a4ab4a53b7af7780f7cbe75d29a0",
              "content": "# Recovered user stories\n\n## US-R-0001 — fixture-symbol\n\n- Recovered capability assertion: `fixture-symbol`\n\n\n## Assertions and inline provenance\n\n| Assertion | Tier | Confidence | Method | Evidence | Extractor | Content hash |\n|---|---|---|---|---|---|---|\n| Capability: fixture-symbol | Deterministic | Confirmed | rule fixture@1 | — | `fixture-rule` | `4df38eddb9b2309bdbaef1c5a20c052949f9ebbbc9022fd1ce1980434b2acaf3` |\n",
              "assertions": [
                {
                  "id": "node:fixture-symbol",
                  "subject_id": "fixture-symbol",
                  "subject_kind": "Capability",
                  "summary": "Capability: fixture-symbol",
                  "provenance": {
                    "tier": "Deterministic",
                    "confidence_tier": "Confirmed",
                    "evidence": [],
                    "extractor_id": "fixture-rule",
                    "content_hash": "4df38eddb9b2309bdbaef1c5a20c052949f9ebbbc9022fd1ce1980434b2acaf3",
                    "method": {
                      "kind": "rule",
                      "rule": "fixture",
                      "version": "1"
                    }
                  }
                }
              ]
            },
            "origin": {
              "kind": "evidence",
              "evidence_ids": [
                "artifact:b232ca3f4a621d605f843f10f8fb65ec19e68cc2080d726376af327919ac40b0"
              ]
            }
          }
        }
      ],
      "source_excerpts": [],
      "interpretations": [],
      "limitations": []
    }

## 4. Work (proposals)

    {
      "affected_components": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "work.affected_components was not supplied"
        }
      },
      "proposed_approach": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "work.proposed_approach was not supplied"
        }
      },
      "dependencies": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "work.dependencies was not supplied"
        }
      },
      "open_decisions": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "work.open_decisions was not supplied"
        }
      }
    }

## 5. Acceptance (proposed verification and distinct observations)

    {
      "positive_cases": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "acceptance.positive_cases was not supplied"
        }
      },
      "negative_cases": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "acceptance.negative_cases was not supplied"
        }
      },
      "proposed_verification": {
        "state": "missing",
        "omission": {
          "class": "missing_input",
          "seam": "interpretation",
          "detail": "acceptance.proposed_verification was not supplied"
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
