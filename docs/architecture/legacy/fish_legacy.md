# Legacy Knowledge: fish

Imported from legacy Qdrant database. These are general facts and rules.

## Memory 13b416c1-b633-41f1-98e4-69fe773c5957
## The Coin Scale Fraud
To prevent forced-perspective length manipulation, the ML measurement pipeline enforces strict camera geometry. The Coin Detector ML model must be trained to identify and reject non-mint objects. If it detects a counterfeit (3D printed/paper) or cannot verify authenticity, it rejects the reference object.

## Memory 217fa9f9-f3b5-4a1b-a4bf-e0c6db72515e
## The Single-Subject Temporal (One Fish)
The ML pipeline enforces a strict 'One Fish, One Coin, One Photo' rule per logged catch. If the YOLO segmenter detects multiple fish in a pile, it rejects the image. A single user cannot log two distinct fish into a bag with the exact same atomic GPS timestamp.

## Memory 2494101b-b93f-4209-b148-dc2980c2e919
## Overview
Handles image and video ingestion, CLIP filtering, and model training for visual fish identification.

## Memory 2eb05687-6803-4cd9-80b1-9c605da4e485
## The Biological Fingerprint (Re-ID)
The app captures and stores the ML embedding (512-float vector) for every Tier 1 catch as forensic evidence for future case tools. If similarity between catches is extremely high (potential duplicate), the app gracefully downgrades the catch: the user can log it, but must explicitly acknowledge that this specific catch is stripped of Bag Insurance.

## Memory 3d501a11-f7a0-4e25-9700-a36e8a51333a
## The Immutable Catch
A Tier 1 (Bag Insurance Eligible) catch cannot be edited. When a Tier 1 fish is verified and placed into a bag, the app must generate a cryptographic hash (fingerprint) of the image data, the ML confidence score, the EXACT GPS COORDINATES (Lat/Lon/Alt/Accuracy), the GPS time, and the SpatiaLite record to prove the record was not altered post-catch.

## Memory 4398132a-0351-4f17-a670-bcbfdb58a115
## Architecture Constraints
- **Language:** Clojure (deliberately chosen for parsing superiority).
- **Execution:** Uses a Hybrid Deterministic-LLM Parsing Pipeline. 
- **LLM Usage:** Uses local/OpenCode LLMs (like `gpt-4o-mini`) via clj-http or shell execution as active pipeline components. Employs a "Critic Pattern" (Double-Pass Verification) to ensure extracted data contains verbatim `source_quote`s and avoids hallucinated numbers. Failed validations fallback to `audit_queue.json`.

## Memory 44e4014b-1911-4675-8b6a-0913d318a2bc
## Overview
The central brain for interpreting fish taxonomy and spatial rule application.

## Memory 472e3db7-ec44-4f5c-967b-c85cee9c1d3b
---
domain: ml_training
description: ML Vision Pipeline. Image/Video ingestion, CLIP filtering, and model training.
governs_paths:
  - ml_training/
---
# ML Vision Pipeline

## Memory 47fac5f0-3f09-4b40-b024-0bcbd55e91d0
---
domain: "core_taxonomy"
description: "Core Logic & Taxonomy. Master taxonomy mappings, most restrictive zone calculations, and measurement anti-fraud."
governs_paths:
  - "nibble_server/internal/taxonomy/"
  - "nibble_server/internal/rules/"
---
# Core Logic & Taxonomy

## Memory 48ba32be-81b3-4dcc-8893-fd2556b85692
Hyperparameter Optimization (HPO) for ML Vision Pipeline must use Optuna. It is lightweight, pythonic, and integrates easily without the heavy footprint of Ray Tune. Targets for tuning: Asymmetric Loss Weights, Augmentation Ranges (ColorJitter, rotation), and Learning Rate/Optimizer Scheduling for MobileNetV3.

## Memory 4f73247c-af43-4738-84e6-b6d123673396
## Architecture Constraints
- **Language:** Python is heavily restricted to this domain ONLY. Do not spread Python to the backend or frontend.
- **Tools:** Utilizes local LLMs as active, integrated pipeline components for Image Filtering.

## Memory 56c5afcb-d4a2-4f3d-8317-6b5b046e594f
## Overview
The data ingestion engine responsible for parsing complex legal text (eCFR, FWC) into structured JSON.

## Memory 5c83810f-75c2-42d4-8b39-20e625f813c8
## The Aggregate Ledger
The app tracks limits hierarchically (Species > Group > Supergroup). When a limit is reached at ANY tier, the target bag is greyed out. The user can create a new bag. The app warns that if the number of bags exceeds the number of licensed anglers onboard, the user assumes all liability. Bag Insurance explicitly does not cover cumulative over-bag charges.

## Memory 5fa8c584-d541-43ca-b2fc-0f48a1b6993a
---
domain: "data_ingestion"
description: "Regulation Data Pipeline. Ingests eCFR + FWC data using Clojure and LLMs."
governs_paths:
  - "data_ingestion/"
---
# Regulation Data Pipeline

## Memory 6fd5ff16-2b41-45b5-8457-a7894155e88b
REST APIs are 100% prohibited. All client-server communication MUST use MQTT. Do not create REST endpoints or api/ directories.

## Memory 7b8cebea-ec51-4eb9-9747-8411a4e88b55
## The Measurement Typology
The ML Fish Segmenter must support distinct FWC measurement modes: Total Length, Fork Length, Lower Jaw Fork Length, and Carapace. The app UI must explicitly instruct the user on how to position the fish. If the YOLO model cannot detect required anatomical landmarks (e.g., tail fork missing), the app cannot grant Tier 1 Bag Insurance.

## Memory 7d1bf198-5fda-4fcd-a9d2-be8b0af69b75
## The Perspective Fraud
The Segmenter must verify the reference coin is placed ON the fish (minimizing depth-of-field distortion). If the coin is closer to the lens than the fish, the app calculates a smaller fish (self-penalizing). If the coin is elliptical (angled shot) instead of circular, the app rejects Tier 1 Bag Insurance.

## Memory 7d2eb588-f3e2-4788-abc6-36a11a441b44
## Overview
The Flutter/Dart client application for the Fish project.

## Memory 85698fa5-c101-4520-9416-d9e2816d7996
---
domain: "nibble_app"
description: "Frontend / Client App. Flutter/Dart app, offline-first via SQLite, syncs regulation updates via MQTT."
governs_paths:
  - "nibble_app/"
---
# Nibble App (Frontend)

## Memory 9e0738ea-4951-4a05-8512-581f7d25f48f
## Architecture Constraints
- **Offline-First:** Relies on local SQLite databases for fast, disconnected access to regulations.
- **Sync Protocol:** Communication with the backend is strictly over MQTT (RabbitMQ). No REST APIs are permitted. The client subscribes to public sync streams to receive regulation updates.

## Memory a30b41ca-b454-45bf-a45b-1fc7c9c78d12
## The 'See No Evil' Transfer
The app does not track physical boat-to-boat transfers or ask questions. If a user deletes a fish from their digital bag, that specific fish's Tier 1 Insurance and forensic proof are permanently destroyed. If a photo is taken of a transferred fish, the app logs it exactly where and when the photo is taken. The app only knows what the camera sees.

## Memory b2b47684-4075-4cd9-8770-e12e52b8fb5d
## Architecture Constraints
- Handles master taxonomy mappings (e.g., WoRMS API resolution).
- Calculates the most restrictive zones when overlapping rules apply.
- Implements measurement anti-fraud logic.

## Memory c9e55fca-d857-48b5-b826-1f989bf4fe48
---
domain: "nibble_server"
description: "Backend Server. Go API, PostgreSQL 18 + PostGIS, RabbitMQ message broker (MQTT)."
governs_paths:
  - "nibble_server/"
---
# Nibble Server (Backend)

## Memory d52b88f1-5ad2-448f-8931-e36e09e13b74
## The Shellfish Visual Exclusion
The ML measurement pipeline (Segmenter & Keypoints) is STRICTLY limited to finfish. Shellfish use a fundamentally different physical measurement metaphor. Any attempt to visually measure or identify shellfish via the camera is explicitly excluded from Tier 1 Bag Insurance and must fallback to manual user input.

## Memory e9571383-167c-4f69-89f7-4b892601d7aa
## The Mutilated Fish (Whole Condition)
FWC law mandates regulated fish must be landed in 'whole condition'. The app outright rejects any catch where the Segmenter cannot find all required anatomical landmarks (e.g., a shark-bitten tail). The app explicitly warns the user that possessing a mutilated regulated fish is illegal and refuses Bag Insurance.

## Memory f8538c8d-52ba-411e-974d-1668a1bb1928
## Architecture Constraints
- **Database:** PostgreSQL 18 with PostGIS for advanced spatial queries.
- **Protocol:** All client-server communication MUST use MQTT (RabbitMQ broker). REST is 100% prohibited across the entire project (internal and external) to avoid temporal reasoning bugs.

