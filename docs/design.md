# ASmart Medical Scribe — System Design

> An on-device application that records doctor–patient conversations, transcribes them locally, and generates structured SOAP-R clinical notes — with no patient data ever leaving the clinician's Windows device.

## Table of Contents

1. [Overview](#1-overview)
2. [Functional Requirements](#2-functional-requirements)
3. [Non-Functional Requirements](#3-non-functional-requirements)
4. [Architecture & Model Selection](#4-architecture--model-selection)
5. [Diagrams](#5-diagrams)
6. [Speech-to-Text Pipeline](#6-speech-to-text-pipeline)
7. [Model Residency Strategy](#7-model-residency-strategy)
8. [Note Generation (LLM)](#8-note-generation-llm)
9. [Data Model & Interfaces](#9-data-model--interfaces)
10. [Security & Compliance](#10-security--compliance)
11. [Trade-offs & Alternatives](#11-trade-offs--alternatives)
12. [Pricing](#12-pricing)
13. [Future Considerations](#13-future-considerations)
14. [Distribution, Updates & Telemetry](#14-distribution-updates--telemetry)

---

## 1. Overview

### Problem statement

Clinicians lose significant time to documentation — writing up each encounter during or after the visit pulls attention away from the patient and extends the working day. Existing AI scribe products are cloud-based, which raises privacy/compliance concerns under Canadian law (PHIPA/PIPEDA) and locks small clinics into recurring per-seat subscriptions.

This product lets the doctor focus on **one thing — treating the patient** — while the application handles documentation. It is **cost-effective** (one-time payment) and **fully private**: all audio capture, transcription, and note generation run on the clinician's own device.

### Goals (in scope for v1)

- Capture doctor–patient conversation audio on the clinician's Windows device for **in-person** consults.
- Transcribe the conversation locally (speech-to-text).
- Generate a structured **SOAP-R clinical note** from the transcript, fully on-device.
- Keep the transcript locally so the doctor can revisit it; the doctor can delete transcripts at any time.
- Run entirely on commodity clinician hardware: **Windows 11, 16 GB RAM or less, no GPU (CPU-only inference)**.

### Non-goals (explicitly out of scope for v1)

- **No EMR/EHR integration** — the doctor copies the note manually into their chart.
- **No billing codes, ICD-10-CA codes, orders, or referrals** — deferred to a future phase.
- **No online/telehealth consults** — v1 captures in-person visits only.

### Key assumptions & constraints

- Target hardware: **Windows 11, 16 RAM or less, CPU-only**. This is the binding constraint on model selection and on whether transcription is real-time vs. post-encounter.
- In-person capture uses a **single microphone** picking up both doctor and patient in the same room.
- **Human-in-the-loop:** the doctor reviews and edits every note; the tool never auto-files anything.
- **Audio is processed, not permanently retained**; the **transcript is retained locally** until the doctor deletes it.
- Market: **Canada** — PHIPA/PIPEDA govern handling of personal health information.

---

## 2. Functional Requirements

The system works like a dictation tool tuned for a clinical visit. While the doctor records, the app transcribes **incrementally**: each time the speaker pauses, the just-spoken segment is transcribed and appended to the on-screen transcript, which the doctor can correct inline at any time (they speak and edit at different moments, never simultaneously). The core loop: **record → see text appear segment-by-segment, → Stop → final transcript review → edit if needed -> click Generate → review/edit the notes → save**. Note generation is **explicit (on click)**, after the doctor is happy with the transcript. Capabilities are prioritized P0 (must-have for v1), P1 (should-have), P2 (deferred/future).

### Capabilities

| # | Capability | Priority | Actor | Trigger | Behavior | Success outcome |
|---|-----------|----------|-------|---------|----------|-----------------|
| FR-1 | **Start/stop recording** | P0 | Doctor | Clicks "Record" at visit start, "Stop" at end | Continuously captures microphone audio for the in-person encounter; shows elapsed time and a recording indicator | A live, growing transcript and a final transcript on Stop |
| FR-2 | **Incremental (segmented) transcription** | P0 | System | Doctor pauses speaking (silence/VAD-detected gap) | Transcribes the just-spoken segment locally and **appends it to the on-screen transcript immediately**, then continues listening for the next segment. | Doctor sees captured text appear segment-by-segment during the visit, with no perceptible wait |
| FR-3 | **Pause/resume recording** | P1 | Doctor | Clicks "Pause" mid-visit | Suspends capture (e.g. patient steps out, private moment) and resumes into the same transcript | Audio excludes paused segments; single continuous transcript |
| FR-4 | **Final transcript review & edit** | P0 | Doctor | After Stop, before generating | Doctor sees the full assembled transcript as a single paragraph and may do a final edit | A doctor-approved transcript ready for note generation |
| FR-5 | **Generate SOAP-R note (on click)** | P0 | Doctor / System | Doctor clicks "Generate Note" | Local LLM produces a structured note with Subjective / Objective / Assessment / Plan / Response sections from the (possibly edited) transcript | A clean, correctly-sectioned SOAP-R note |
| FR-6 | **Review & edit note** | P0 | Doctor | Note displayed | Doctor reads and freely edits any section before use (human-in-the-loop; nothing is auto-filed). May go back, edit the transcript, and regenerate if the note is badly wrong | Doctor-approved note text |
| FR-7 | **Mic device selection & level check** | P1 | Doctor | Before/at recording | Choose input device and see a live input-level meter to confirm audio is being captured | Confidence that the right mic is working before the visit |
| FR-8 | **Browse & reopen saved encounters** | P1 | Doctor | Opens the saved-encounters list | Lists previously saved encounters (timestamp/label) and lets the doctor reopen a transcript/note to view, edit, re-export, or delete | Doctor can return to past notes without an external system |

### Session & retention model

- **Transcripts and notes are persisted locally inside the application** and remain available across sessions until the doctor deletes them. The app keeps a local store of past encounters the doctor can revisit.
- **Audio is transient**: held only long enough to transcribe each segment, then discarded. Audio is never written to disk as a retained file.
- All persisted PHI (transcripts and notes) is **encrypted at rest** (see NFRs) so a lost or stolen device does not expose patient data.

### Edge cases

- **Silence / no speech**: produce an empty or "insufficient audio" result rather than a hallucinated note.
---

## 3. Non-Functional Requirements

All targets are for the **binding hardware profile**: Windows 11, 16 GB RAM or less, **CPU-only, no GPU**. Numbers are design targets to validate during benchmarking, not guarantees, given on-device model variability.

| # | Requirement | Target | Rationale |
|---|------------|--------|-----------|
| NFR-1 | **Per-segment transcription latency** | Captured text appears **< 2 s** after a speech pause (for a typical 5–15 s utterance) | Must feel near-instant so the doctor isn't waiting mid-visit; drives choice of a fast, CPU-light STT model (Parakeet TDT v3, §6.4) |
| NFR-2 | **Note generation** | Runs in a **background queue**; doctor is not blocked and can start the next patient. Target completion **< 90 s** for a ~20-min encounter on the 16 GB profile | A 7–8B quantized LLM on CPU needs time; backgrounding hides it so throughput isn't affected |
| NFR-3 | **Encounter length** | Handle encounters up to **~30 min** of audio without instability | Headroom over the ~20-min average; long visits must not exhaust memory |
| NFR-4 | **Peak memory** | Total app + models peak **< 12 GB RAM** | Leaves ~4 GB for Windows + the doctor's EHR/browser on a 16 GB machine; STT + LLM may need to load/unload to coexist |
| NFR-5 | **Encryption at rest** | All persisted PHI (transcripts, notes, app store) **encrypted at rest** (AES-256; key protected via Windows DPAPI tied to the user account) | Persistent local PHI must survive device loss/theft without exposure |
| NFR-6 | **Durability / crash safety** | Captured transcript persisted incrementally so an app/OS crash mid-visit loses **≤ the last unsaved segment** | A 20-min visit's transcript must not vanish on a crash |
| NFR-7 | **Data lifecycle** | Audio discarded immediately after each segment is transcribed; transcript/note retained until the doctor deletes them; deletions are **permanent** (no recycle/cloud copy) | Minimizes audio PHI footprint; gives the doctor full control over retained text |
| NFR-8 | **Note quality** | No hard WER SLA in v1. Commit: **all SOAP-R sections populated only from transcript facts, no fabricated content**; **mandatory human review** before use | Honest given CPU-model limits; safety comes from the human-in-the-loop, not model perfection |
| NFR-9 | **Availability** | N/A as a service (local desktop app); target **no crash** across a full clinic day; graceful recovery on restart | It's a local app |
| NFR-10 | **Install & footprint** | Single Windows installer; models downloaded once after installation; on-disk footprint **target < 10-20 GB** (STT + LLM weights) | Must be deployable by a non-technical clinic on a normal laptop |
| NFR-11 | **Cold start** | App ready to record **< 10 s** from launch (models may lazy-load on first record) | Doctor can't wait minutes between patients |

---

## 4. Architecture & Model Selection

### Component overview


| Component | Responsibility | 
|-----------|----------------|
| **Audio capture** | Read microphone, buffer PCM, detect speech pauses (VAD) to segment utterances
| **STT engine** | Transcribe each audio segment to text; EN/FR auto-detect
| **Transcript store** | Hold the live, editable transcript; persist per-encounter; preserve manual edits
| **Note generator (LLM)** | Turn the approved transcript into a structured SOAP note (EN/FR), on click
| **Prompt/template layer** | SOAP-R system prompt, section schema, language handling, anti-fabrication guardrails
| **Local store** | Encrypted persistence of transcripts + notes; saved-encounter list
| **UI shell** | Record controls, live transcript, note view/edit, export/print, saved list

### Model selection — note generator (the focus of v1)

- **Runtime:** `llama.cpp` (GGUF), CPU inference, 4-bit quantization (Q4_K_M) to fit memory and hit latency.
- **Default model:** **Mistral-7B-Instruct v0.3** — Apache-2.0, heavy, best note generation.
- **Alternate model 1:** **Phi3.5 Q_8** — MIT, lighter, decent note generation.
- **Alternate model 2:** **Phi3.5 Q_4** — MIT, lightest, okayish note generation.
- **Swappable interface:** the note generator sits behind an internal `generate_note(transcript, language) -> SOAP-R` interface so the model can be upgraded or A/B-tested without touching the rest of the app (NFR-14).

---

## 5. Diagrams

### 5.1 System context

Shows who uses the app and the hard boundary of the clinician's device. The only external system is the EHR, reached **manually** via clipboard/file by the doctor — the app never talks to it.

```mermaid
flowchart TB
    Doctor([👩‍⚕️ Doctor])
    Patient([🧑 Patient])

    subgraph Device["🔒 Clinician's Windows 11 Device (privacy boundary)"]
        App[ASmart Medical Scribe App<br/>capture · transcribe · generate · store]
        Store[(Encrypted local store<br/>transcripts + notes)]
        App --- Store
    end

    EHR[/External EHR / EMR<br/>separate system/]

    Patient -- speaks --> Doctor
    Doctor -- "voice (in-person)" --> App
    Doctor -- "reviews & edits" --> App
    Doctor -- "manual copy / paste / export" --> EHR

    App -. "NO network egress of PHI" .-x Cloud((☁️ Cloud))
```

### 5.2 Component diagram

The internal building blocks (Section 4). The STT path is the existing OSS component; the note-generation path is what this project primarily builds.

```mermaid
flowchart LR
    Mic[🎤 Microphone] --> Cap[Audio Capture + VAD]

    subgraph STT["STT path"]
        Cap --> Seg[Segment buffer]
        Seg --> ASR[STT Engine]
    end

    ASR --> TStore[Transcript State<br/>live, editable, edits preserved]

    subgraph NoteGen["Note-generation path — built by this project"]
        TStore --> Prompt[Prompt/Template Layer<br/>SOAP schema · language · anti-fabrication]
        Prompt --> LLM[LLM Note Generator]
        LLM --> Note[SOAP Note]
    end

    TStore --> UI
    Note --> UI[UI Shell<br/>record · transcript · note · export]
    UI <--> Persist[(Encrypted Local Store<br/>AES-256 + DPAPI)]
```

### 5.3 Sequence — live capture → note generation

The core runtime flow: incremental transcription during the visit, then on-click note generation, then the delete-transcript prompt.

```mermaid
sequenceDiagram
    actor Dr as Doctor
    participant UI as UI Shell
    participant Cap as Capture+VAD
    participant ASR as STT Engine
    participant LLM as LLM Generator
    participant DB as Encrypted Store

    Dr->>UI: Click Record
    activate UI
    loop For each spoken segment (until Stop)
        Dr->>Cap: Speaks
        Cap->>Cap: Detect pause (VAD)
        Cap->>ASR: Send segment audio
        ASR-->>UI: Segment text (< 2s)
        UI-->>Dr: Append text to transcript
        opt Doctor corrects
            Dr->>UI: Inline edit (preserved)
        end
        UI->>DB: Persist transcript incrementally
    end
    Dr->>UI: Click Stop
    Dr->>UI: Final transcript review/edit
    Dr->>UI: Click Generate Note
    UI->>LLM: generate_note(transcript, language) [background]
    Note over LLM: STT may unload to free RAM (<12GB)
    LLM-->>UI: SOAP note
    UI-->>Dr: Show note for review/edit
    UI->>DB: Save note (encrypted)
    UI-->>Dr: Prompt: delete transcript?
    alt Doctor keeps
        DB->>DB: Transcript retained
    else Doctor deletes
        UI->>DB: Permanently delete transcript
    end
    Dr->>UI: Copy / Export / Print
    deactivate UI
```

---

## 6. Speech-to-Text Pipeline

This section details the on-device speech-to-text (STT) subsystem — the path from raw microphone input to editable transcript text. It is implemented in the **Rust backend** of the Tauri application and runs entirely locally. The pipeline is described as a sequence of discrete stages.

### 6.1 Audio capture

The capture stage turns live microphone input into a clean, uniform audio signal that the rest of the pipeline can rely on. It records **all** incoming sound — deciding what is speech versus silence is a later stage (VAD, §6.2), not capture's job.

**Design:**

- **Dedicated capture thread.** Audio capture runs on its own thread inside the app process (not a separate process), parallel to the UI. Its single job is to pull audio frames from the microphone as they arrive and forward them downstream over a thread-safe channel (mpsc). Isolating capture on its own thread guarantees that UI work (rendering, button clicks) can never stall capture and cause dropped audio. The thread is active for the duration of a recording and parked when idle.
- **Cross-platform audio I/O via `cpal`.** The app opens the OS default or a user-selected input device and reads its **native format** — whatever sample type (e.g. `u8`, `i16`, `f32`), sample rate, and channel count the hardware happens to provide.
- **Normalize to a uniform signal.** Every captured sample is converted to **32-bit float (`f32`)**, and the stream is **resampled to 16 kHz mono** (via `rubato`).
  - *`f32`* — one uniform numeric format downstream, normalized to the −1.0…+1.0 range, avoiding precision loss in subsequent math.
  - *16 kHz* — human speech information lives below ~8 kHz; by the Nyquist limit, 16 kHz sampling captures all of it. Higher rates (44.1/48 kHz) only add data the model doesn't need, increasing compute for no accuracy gain. 16 kHz is the minimum rate that fully preserves speech.
  - *mono* — a single combined channel; stereo is redundant for transcribing speech and doubles the data.

  Normalization changes only the *format/resolution* of the audio, never its content — all sounds (voices, noise, silence) are still present, just represented efficiently.
- **Live input-level feedback.** A lightweight tap on the capture stream computes the current input amplitude and pushes it to the UI to drive a live waveform / volume meter. This is purely UX — it lets the clinician confirm the microphone is live and picking up sound **before** committing a full visit to recording (supports FR-12).

### 6.2 Voice activity detection (VAD)

The VAD stage consumes the clean 16 kHz stream from §6.1 and classifies it, frame by frame, as **speech** or **silence/noise**. Its output serves two purposes: it **drops non-speech audio before the model sees it** (improving accuracy and speed), and it **defines segment boundaries** — where one spoken utterance ends and the next begins — which drives the transcription trigger (§6.3).

**Design:**

- **Neural VAD (Silero).** A small, CPU-cheap neural model classifies audio by the acoustic signature of human speech, rather than by raw loudness. This is deliberately chosen over a simple energy/volume threshold: an exam room has door slams, keyboard noise, and HVAC hum that a loudness threshold would misfire on, while quiet talking would be missed. Silero ignores loud non-speech and still catches soft speech, making it robust to real clinical background noise.
- **Frame-by-frame probability.** Audio is fed in **30 ms frames**; for each, Silero emits a speech **probability** (0.0–1.0). A configurable **threshold** (default ~0.5) converts this to a speech/noise decision, so noisy rooms can be tuned without code changes.
- **Smoothing layer.** Raw per-frame decisions are too jittery to define segments directly, so the VAD is wrapped in a smoothing layer with three controls:
  - **Onset** — requires N *consecutive* speech frames before declaring speech has started, so brief blips (a cough, a clink) don't open a spurious segment.
  - **Hangover** — after speech appears to stop, keeps the segment open for N more frames; only a *sustained* silence ends it. This prevents a natural mid-sentence pause from prematurely chopping a segment.
  - **Prefill (pre-roll)** — buffers the few frames immediately *before* onset fired and prepends them to the segment, so the leading syllable of the first word isn't clipped.
- **Clinical tuning bias.** Defaults lean toward a **longer hangover**: clinicians and patients pause mid-thought, and the system should prefer keeping an utterance whole over fragmenting it. Exact onset/hangover/prefill/threshold values are set during benchmarking against real clinical audio.

> Note: the hangover/sustained-silence duration that closes a segment is the same signal that defines a **segment boundary**, which is the transcription trigger detailed in §6.3.

### 6.3 Segment buffering & transcription trigger

This stage answers: **when is audio handed to the STT model, and how often?** The answer defines the live, incremental transcription experience (FR-2). All of this runs inside the single app process, across parallel threads.

**Why not transcribe once at the end.** The simplest approach — accumulate the whole recording and transcribe once on Stop — is unacceptable for a ~20-minute encounter: the doctor would see nothing until Stop, then wait through one large transcription, with the entire visit's audio held in memory. Instead, the system transcribes **one segment at a time**, cutting at the natural pauses that VAD (§6.2) already detects, so the transcript grows line-by-line during the visit.

**Design:**

- **Accumulate the current segment.** Speech frames passed by VAD append to a current-segment buffer; silence/noise frames are dropped.
- **Close & flush on a pause boundary.** When VAD reports sustained silence (hangover expires), the current segment is complete: its audio is flushed as one finished segment, the buffer is cleared, and accumulation resumes for the next segment.
- **Decoupled threads via a queue.** The capture (audio) thread never runs the model. A finished segment is pushed onto a thread-safe queue (mpsc channel); the capture thread immediately resumes listening. A separate **transcription worker thread** pulls segments from the queue and runs STT (model kept warm — §6.4). This decoupling guarantees that a slow transcription can never stall capture or drop audio (NFR-1).

- Mental model: 
    - audio thread = ears (always listening)
    - transcription thread = hands (typing it out)
    - queue = conveyor belt between them.

- **Ordered assembly.** Because transcription is asynchronous, each segment carries a **sequence number** so the UI appends results in spoken order (FR-2/FR-3), regardless of completion timing.
- **Tail flush on Stop.** On Stop, any still-open segment is force-flushed so the final words are transcribed and not lost.

**Safeguards:**

| Safeguard | Problem it solves | Design |
|-----------|-------------------|--------|
| **Max-segment cap** | A speaker who talks continuously with no real pause never triggers a boundary, producing one oversized segment that breaks latency and grows memory | Force-flush the current segment after a maximum duration (≈20–30 s) even without a pause boundary, bounding latency (NFR-1) and memory (NFR-5) |
| **Min-segment floor** | Tiny blips create useless sub-second fragments | VAD onset filters most; additionally discard segments below a minimum length |

**Trade-off accepted:** transcribing per-segment gives live feedback but means the model sees one utterance at a time and loses cross-segment conversational context. For the Parakeet model this is a minor accuracy cost, accepted in exchange for the live incremental UX that FR-2 requires.

```mermaid
flowchart LR
    Mic[🎤 Mic]

    subgraph AudioThread["🎧 Audio thread — always listening"]
        direction TB
        Cap[Capture + VAD] --> Buf[Current-segment buffer]
        Buf -->|"VAD pause / max-cap"| Cut([Cut segment #N])
    end

    subgraph TransThread["⌨️ Transcription thread"]
        direction TB
        TW[STT model] --> Txt[Text + seq #]
    end

    Mic -->|speech frames| Cap
    Cut -->|enqueue| Q[(Queue<br/>mpsc channel)]
    Q -->|dequeue| TW
    Txt --> UI[🖥️ UI — append in spoken order]
```

### 6.4 STT engine & model lifecycle

This stage answers: **which model runs, and when does it live in RAM?** The lifecycle is where the system honors the memory budget (NFR-5, <12 GB peak) without paying a model-load cost on every segment.

**The engine.** Transcription runs through a Rust STT engine that executes the Parakeet model locally on CPU via an ONNX runtime. It sits behind a narrow, swappable interface (`transcribe(audio) -> text`), so the underlying model can be changed without touching the capture, VAD, or assembly stages (NFR-14).

**Model** (see §4 for selection rationale):

| Role | Model | License | Notes |
|------|-------|---------|-------|
| Sole engine (v1) | Parakeet TDT 0.6B v3 | CC-BY-4.0 (attribution required) | Fast, CPU-light, multilingual EN+FR with auto-detect; the all-rounder default. v1 ships this as the only STT engine |

Models are downloaded once on first selection and cached on disk thereafter.

**Lifecycle — "warm during use, released when idle":**

- **Warm for the whole encounter.** Once loaded, the model stays resident in RAM across *every* segment of the visit. The per-segment cost is therefore inference only — no repeated loading — which is the single biggest contributor to keeping segment latency low (NFR-1).
- **Background preload on app open.** The app window paints instantly (cold start <10 s, NFR-13); immediately after, a background thread begins loading the model so it is warm by the time the clinician presses Record. The UI is never blocked on the load. Recording is the app's primary purpose, so preloading — rather than waiting for the first Record — hides the one-time disk read (≈1–3 s to read the model file off SSD into RAM) behind the app-open moment.
- **Idle-unload via a watcher thread.** A background watcher periodically checks the time since the model was last used; past a configurable idle timeout it unloads the model and frees the RAM, returning memory to the clinician's other applications between patients. The watcher never unloads a model that is mid-recording.
- **Reloads are cheap.** After a model has been loaded once, the OS keeps its file pages in the disk cache, so a reload shortly after (e.g. the next patient) is near-instant — it reads from RAM-cached pages rather than the SSD. Only sustained idleness incurs a full disk read again.
- **Safe concurrency.** The resident model is guarded so a Record action and an idle-unload cannot collide; loading is coordinated so two triggers can't load it twice.

### 6.5 Transcript assembly & delivery to UI

This stage answers: **how does a finished segment reach the screen — live, in order, without clobbering edits the clinician has already made?** (FR-2 live transcription, FR-3 editable transcript.)

**Push, not pull.** The transcription worker produces, per segment, `{ sequence_number, text }`. It crosses the Rust↔webview boundary by **emitting an event**; the frontend **listens** and appends. The backend never renders the transcript and does not hold a master copy that the UI mirrors — it only announces finished segments.

```
Worker thread (Rust) ── emit("transcript-segment", {seq, text}) ──► React listener ──► append to editor
```

**Asynchronous and non-blocking.** Neither side waits on the other: Rust emits and immediately returns to its work; the React listener reacts whenever an event arrives. This is what keeps capture and transcription from ever stalling on the UI (NFR-1).

**Ordered append.** Because segments finish asynchronously (a short utterance can transcribe before an earlier long one), each carries the **sequence number** assigned in §6.3. The UI maintains an ordered-by-sequence list and places each segment at its correct position, so the displayed order always matches the spoken order regardless of completion timing. Each segment is delivered **once, already final** — there is no mid-segment partial/streaming text, which avoids flicker at the cost of one-utterance (rather than word-by-word) granularity.

**Edit preservation (FR-3) — append-only backend, frontend owns the document.** The clinician can edit the transcript while recording continues (correcting a drug name, a dosage). The rule that protects those edits:
- New segments are **only ever appended at the tail**; the backend never rewrites or re-inserts into already-delivered text.
- Once a segment is in the editor, it belongs to the **frontend's editable document**, which is the source of truth — the backend has no channel to mutate prior lines.
- Consequently an edit to an earlier segment is untouched when a later segment arrives; they are disjoint regions and the backend writes only to the growing tail.
- The full transcript is therefore **never re-sent** (which would wipe edits) — only incremental appends are emitted.

**Backend copy is backup only.** The backend may retain a raw transcript copy purely for crash-recovery / autosave, but that is a backup — not the artifact the UI mirrors. The editable truth lives in the frontend.

### 6.6 Threading & coordination (orchestration)

This stage answers: **what ties Pieces 6.1–6.5 together into a single, well-behaved lifecycle?** It adds no new audio or STT component — it is the **coordinator** that owns application state and spins the three threads up and down cleanly so nothing leaks and nothing is lost.

**The state machine.** A recording encounter moves through three states, owned by the backend:

```
        Start                Stop
 IDLE ─────────► RECORDING ─────────► PROCESSING ─────► IDLE
  ▲   (spin up)             (drain & finalize)            │
  └───────────────────────────────────────────────────────┘
```

- **IDLE** — app open, the STT model preloaded/warm in the background (§6.4), no capture running.
- **RECORDING** — the capture thread and transcription worker are both live and running asynchronously in parallel; segments flow through the queue and finished text is emitted to the UI (§6.3, §6.5).
- **PROCESSING** — Stop has been pressed: capture has ended, but in-flight audio is still being finalized. Brief in v1; this is also the lifecycle slot where phase-two note generation will run.

**The three threads.**

- **Capture thread** (the "ears") — owns the cpal stream and VAD; emits audio *segments* into the queue.
- **Transcription worker** (the "hands") — pulls segments off the queue, runs the STT engine, and emits finished text to the UI.
- **UI thread** — renders the editable transcript and owns the Start/Stop controls.

The two hops differ by design: **capture → transcription is an mpsc queue** (a real buffer that can hold a backlog), while **transcription → UI is a push event** (§6.5), not a buffer the UI drains.

**Who owns Start/Stop.** The UI *requests* transitions across the bridge (`invoke("start_recording")` / `invoke("stop_recording")`, triggered by a button or hotkey). The **backend coordinator owns the actual state** and decides whether a transition is legal. **State guards** reject illegal or duplicate transitions (a second Start while already RECORDING, or a Start during PROCESSING) so rapid clicks or hotkey spam can't corrupt the machine.

**Start — spin up.** On `IDLE → RECORDING`: ensure the model is loaded (normally already warm from preload; otherwise load with a brief "loading…" state), open the mpsc queue and wake the transcription worker, then start the capture thread. Capture and transcription now run in parallel; the coordinator returns immediately and does **not** block for the duration of the recording.

**Stop — drain & finalize.** On `RECORDING → PROCESSING`, order matters so no audio is lost:

1. Signal the capture thread to stop and **tail-flush** the open segment (§6.3) into the queue.
2. Let the worker **drain the queue** — transcribe every remaining segment and emit it.
3. Once the last segment has been emitted to the UI, transition to **IDLE**.

The model is **not** unloaded here in v1 — it is left warm, and the idle-watcher (§6.4) releases it later if the app sits unused. *(Phase two: PROCESSING is where the STT model is unloaded and the LLM loaded for note generation.)*

**Clean teardown & resilience.** Threads are stopped via a signal and **joined** (or parked for reuse) and the queue is closed, so no orphaned threads survive between encounters. If a thread **panics** (e.g. a model error), the coordinator catches it, surfaces an error to the UI, and returns the machine to a safe **IDLE** rather than wedging.

### 6.7 Post-ASR correction suggestions

This stage answers: **how do we help the clinician catch the mishearings that fuzzy word-fixing (§6.3) can't — errors that only a reader who understands the sentence would spot?** Deterministic cleanup corrects a single garbled word against a known list; it cannot repair a phrase that was transcribed as fluent-but-wrong English. 

**Suggest, never rewrite.** The correction pass proposes edits; it does **not** change the transcript on its own. Each suggestion is surfaced in the UI and applied only if the clinician **accepts** it. This is the property that keeps the feature safe: the machine never silently alters what was said, so it cannot introduce a clinical fact the clinician didn't approve. It is the same anti-fabrication discipline as note generation (§8.3), enforced here by a human-in-the-loop gate rather than by a prompt alone.

**When it runs — sequenced, on Stop, before Generate.** The pass is triggered automatically when recording stops, and slots into the review step of §8.1 (between Stop and Generate):

```
Stop ─► correction suggestions (auto) ─► clinician accepts/rejects ─► clicks Generate ─► note
```

**Reuses the resident model.** Correction uses the **same note-generation LLM** already governed by the residency strategy (§7).
The cost is one extra inference over the transcript; because it is **prefill-heavy but decode-light** (long input, short structured output), it is meaningfully cheaper than the note itself, and it overlaps the clinician's own reading, so its latency is largely hidden.

**Streamed, parse-as-you-go.** The model emits suggestions as a stream of small, independently-parseable units (one structured record per line), each naming an **original span** and its **replacement**. The UI shows each suggestion the instant its record completes, rather than waiting for the whole pass — so the first corrections appear early and perceived latency drops. Constraining the output to *replacements of spans that exist in the transcript* (not free-form text) is what keeps a "suggestion" from becoming an invention.

**Non-blocking presentation — a review list beside the transcript.** Suggestions appear as a **list next to the transcript**, each row showing the flagged phrase and its proposed replacement with Accept / Reject in place, without obscuring the transcript. Accepting replaces that span in the editor and rides the existing debounced autosave (§6.5); rejecting dismisses it. The clinician can also ignore the whole pass — a **Cancel** path (reusing the generation cancel mechanism) lets them skip straight to editing and Generate.

**Behavioral rules.**

- **Duplicate spans** — if the same original phrase occurs more than once, a suggestion applies to the **first not-yet-accepted** occurrence. (Rare; a simple, predictable rule.)
- **No suggestions** — if the pass finds nothing, nothing pops up and Generate simply becomes available. No error, no modal.
- **Failure/cancel** — a model error or a Cancel returns to a plain editable transcript; the feature is strictly additive and never blocks note generation.

---

## 7. Model Residency Strategy

The application runs two models on the same machine: the speech-to-text model used during recording, and the note-generation (LLM) model used after recording stops. Both are sizable, CPU-only, and resident in RAM while in use. On a roomy machine they can stay loaded **at the same time**, so the hand-off from transcription to note generation is instantaneous. On a tighter machine, keeping both resident risks pushing the operating system into **disk paging**, which degrades the whole system unpredictably — worse than a deliberate, momentary model reload. The residency strategy decides, per device, which of these two regimes to use.

This section covers **only the one-time mode decision**. Run-time concerns — checking momentary free RAM right before a generation, loading/unloading on the swap path, and graceful degradation — belong to the lifecycle/orchestration design and are out of scope here.

### What we measure

At first run we read the machine's **total physical RAM** (via a system-info probe). Total RAM is a stable, per-device property: it does not change between sessions and it alone determines whether co-residency is *ever* viable on this box. We deliberately do **not** base the mode on momentary *available* RAM, which fluctuates with whatever else the user has open and would make the decision flip-flop.

### The feasibility calculation

We compute a combined footprint:

```
footprint = STT model size
          + selected LLM quantized size      (supplied by the model-choice design; not assumed here)
          + headroom for app + webview + OS   (~2–3 GB)
```

The LLM size is a **parameter** read from the chosen note-generation model, not a value baked into this strategy — so the logic holds regardless of which model and quantization are selected.

### The two modes

| Mode | Behavior | Cost | When chosen |
|------|----------|------|-------------|
| **Co-resident** | Both STT and LLM stay warm in RAM throughout a session | Higher steady-state RAM use | Comfortable margin available |
| **Swap** | One model resident at a time; the LLM is loaded at the transcription→generation hand-off | A few seconds of model-load latency at hand-off | Tight machine, no safe margin |

Co-resident gives a zero-latency hand-off from Stop to note generation. Swap trades that latency for a substantially lower peak RAM footprint.

### Margin, not bare fit

The mode is chosen on **headroom, not technical fit**. We require a real buffer (≈2 GB free above the combined footprint) before selecting co-resident. Bare-fit ("it just barely fits") invites disk paging the moment any transient memory pressure appears, so we treat it as not fitting.

### Decide once, cache it

Because this is a per-device property, we probe **once on first run**, choose the mode, and **persist it** to the settings store alongside the total-RAM value we observed. Subsequent launches read the cached mode rather than re-probing. The single trigger for re-evaluation is a **hardware change**: if the stored total-RAM value no longer matches the current machine, we re-probe and re-cache. There is no per-launch probe.

### Manual override

The automatic decision is the default, but the user can force a mode in settings — e.g. force **Swap** to keep RAM free for other applications, or force **Co-resident** on a borderline machine they know performs fine. An explicit override takes precedence over the cached automatic decision.

---

## 8. Note Generation (LLM)

Phase two turns a verified transcript into a structured clinical note. This section is built up piece by piece.

### 8.1 Trigger & input

Note generation is **manual and explicit**, not automatic on Stop. The sequence is:

1. **Stop** finalizes the transcript. In-flight audio is flushed and the last segments land in the transcript (the Processing→Idle drain described in §6.6). The complete transcript is shown in the UI and the machine is back at rest — no model is running.
2. **The clinician reviews and edits the transcript**, assisted by the post-ASR correction suggestions that ran automatically on Stop (§6.7).
3. **The clinician clicks Generate.** *This* is the trigger that starts note generation, operating on the transcript exactly as the user left it.

Making generation an explicit, post-review action is a deliberate clinical-safety choice: the clinician verifies the source text before a note is built from it, and the expensive LLM step is decoupled from recording.

**Input.** Generation receives the **plain transcript text, as edited** — a flat text stream. It carries:

- **No speaker labels.** The speech-to-text models transcribe words only; they do not identify speakers. Speaker attribution ("who spoke when") is a separate task (diarization) requiring an additional, error-prone model that would compete for the memory budget in §7. For a two-party encounter the note-generation model infers role from content well enough, so v1 sends flat text and defers diarization to Future Considerations.
- **No extra metadata.** No visit type, specialty, or patient identifiers are sent to the model. The clinically relevant content is already in the transcript; the encounter date is stamped by the app at save time, not by the model.

**Regeneration & versioning.** Each press of **Generate** produces a **new note version** tied to the encounter; previous versions are **retained and revertable**. A clinician who prefers an earlier generation can fall back to it. (The storage mechanics live in Piece 6 — Delivery & persistence.)

**Editing.** The generated note is **editable directly in the application.** The clinician revises the model's output in place, and the edited text is what gets saved as the final note. Combined with versioning, the user may also revert to an earlier generated version and edit that one instead.

**Guard.** Generate is **disabled when the transcript is empty** and is **only available in the Idle state** — never mid-recording.

### 8.2 Model & runtime

**Model selection.** All candidates were evaluated for raw note quality and judged acceptable, so selection is purely a fit-to-machine policy keyed on total RAM. This decision is made at the **same one-time startup probe** that drives the residency strategy (§7): the probe reads total RAM, this rule picks the model, and §7 then feeds the chosen model's size into its footprint calculation to decide co-resident vs swap.

| Detected total RAM | Model | Default quant | User override |
|--------------------|-------|---------------|---------------|
| **≥ 16 GB** | Mistral-7B-Instruct-v0.3 (Apache-2.0) | Q4_K_M (~4.4 GB) | — |
| **< 16 GB** | Phi-3.5-mini-instruct (MIT) | **Q8_0** (~4.0 GB) | switch to **Q4_K_M** (~2.3 GB) if generation feels slow |

**Execution model.** The selected GGUF model runs **in-process** inside the Rust backend via the `llama-cpp-2` binding to llama.cpp — no separate inference server, no external process, and no network calls. This keeps all note generation fully on-device, satisfying the zero-egress requirement (NFR-6).

**Model distribution & first-run setup.** The installer ships **no** model weights — it carries only the application (and the small VAD model), keeping the download lean. The models the app needs are fetched **once, on first launch**, through a one-time **Setup** step, then cached on disk and reused every launch — fully offline thereafter (matching the STT lifecycle in §6.4, "downloaded once on first selection and cached"). Setup downloads exactly the models this machine requires: the **RAM-fit note-generation model** chosen by the rule above and the **Parakeet STT model**. Beyond that required pair, the other note-generation tiers remain available as on-demand downloads from Settings, so the clinician can switch model later.

- **Gated until ready.** On launch the app checks whether the required models are present; if not, it shows the Setup screen and does not proceed into recording/generation until both are downloaded and verified. Once present, Setup is skipped entirely.
- **Integrity-checked.** Each download is verified against a known checksum before it is accepted, so a corrupted or truncated transfer is rejected rather than loaded.
- **Not PHI egress.** These are model-weight downloads on first run, the only outbound network calls in the app; no patient data ever crosses the device boundary (NFR-6). After Setup the app runs with no network dependency for core function (§4 deployment model 1).

**Tuning notes (recorded, set at implementation via benchmarking — not architectural decisions):**

- **Thread count** — scaled to the machine's physical core count.
- **Context window** — capped to a working size covering the longest realistic transcript + prompt + generated note, to avoid reserving RAM the §7 budget needs.
- **Sampling** — low temperature for near-deterministic, low-hallucination clinical output (finalized alongside the prompt in §8.3).
- **Memory levers** — mmap vs full load, and KV-cache precision, available as RAM/latency trade-offs during tuning.
- **Prompt caching (fixed prefix reuse).** The system prompt + one-shot example (§8.3) are byte-identical every generation; ordering them ahead of the transcript lets their KV cache be prefilled once and reused across notes instead of re-read each time. Full design in **§8.6**.

### 8.3 Prompt & output structure

**Output format — markdown.** The model emits the note as **markdown** with five fixed section headers (`## Subjective`, `## Objective`, `## Assessment`, `## Plan`, `## Response`) — the SOAP-R structure. Markdown is the single representation used everywhere:

- **Display** — rendered as a formatted document in the UI (like a markdown preview), so the clinician sees an ordinary-looking note rather than raw `##`/`**` markers.
- **Edit** — the clinician edits in-app (§8.1); the note stays markdown throughout.
- **Store & version** — markdown is plain text, so persistence and versioning (§8.5) are trivial.

**Input — whole transcript, single prompt.** The full transcript is passed in one prompt with no context-handling layer (no chunking or pipeline); structured SOAP output comes from prompt engineering alone, since the window far exceeds a realistic consult.

**Scope — five sections (SOAP-R).** v1 produces **Subjective / Objective / Assessment / Plan / Response**.
- Subjective(S) — patient's reported symptoms, feelings, history
- Objective(O) — measurable/observed data (vitals, exam, results)
- Assessment(A) — clinician's diagnosis/interpretation of S+O
- Plan(P) — next steps (treatment, meds, referrals, follow-up)
- Response(R) — how the patient responded to prior treatment since last visit

**Bulleted, concise output.** Sections are written as **concise bullet points, not paragraphs**.

**Empty sections.** A section the transcript has no material for is written as **"Not discussed"**.

**Prompting approach — one-shot.** The prompt carries **one worked example** (a raw, messy consult transcript and its ideal bulleted SOAP-R note). This locks structure and style far more reliably than instructions alone.
- **Few-shot** (a handful of examples) remains a deferred lever if one example proves insufficient. The one-shot example is a **fixed prefix**, prompt-cached (§8.2) so it adds negligible per-note latency.

### 8.4 Lifecycle & orchestration

This section covers how a generation runs end to end: where it sits in the app's state machine, when the model is loaded, and how warmup, cancellation, and failure are handled.

**State machine.** Generation is a manual action from IDLE (the transcript is already finalized and editable, §8.1), so it is a distinct state rather than part of STT processing:

```
IDLE ──Generate──► GENERATING ──complete / cancel / fail──► IDLE
```

While in GENERATING, recording is blocked and a second Generate is ignored — a single generation is in flight at a time. On any exit (success, cancel, or failure) the app returns to IDLE with the transcript preserved intact.

**Model load timing — driven by the §7 residency mode.** The startup probe (§7) has already decided co-resident vs. swap; this section consumes that flag:

| Mode | When LLM is loaded | On Generate | After generation |
|------|--------------------|-------------|------------------|
| **Co-resident** | At startup, alongside STT; stays resident | Already in RAM — generation starts immediately | Stays resident |
| **Swap** | Lazily, per generation | Unload STT → load LLM → generate | Unload LLM → reload STT for the next recording |

In swap mode the load/unload is the RAM cost of running on a tight machine; in co-resident mode that cost is paid once at startup.

**Warmup.** The first inference after a model load is slower (cold weights, cold buffers). To keep the clinician's first real generation at full speed, a hidden warmup pass — a tiny throwaway generation, never shown — runs immediately after each load: at startup in co-resident mode, and right after the swap-load in swap mode.

**Cancellation.** A Cancel control stops generation mid-stream (via the decode loop's stop hook). On cancel, the partial note is **discarded** and the screen returns to its pre-Generate state; the transcript is untouched. Streamed output (below) keeps this responsive — the user sees tokens appear, so a cancel feels immediate.

**Streaming.** Tokens are streamed to the UI as they are produced rather than shown only when complete. This makes the wait *feel* short and makes cancellation feel instant. 

**Load-time RAM guard.** The §7 budget is a startup decision on *total* RAM; actual *available* RAM at generation time can be lower. Before loading the LLM, available RAM is checked. If it is insufficient, the load fails gracefully: the app surfaces the error, stays in IDLE, and preserves the transcript — never a silent out-of-memory crash.

### 8.5 Delivery & persistence

This section covers how the streamed note reaches the screen and how it is stored durably and encrypted at rest.

**Streaming render.** The backend emits generated tokens as events to the frontend (§8.4); the frontend appends them to a buffer as they arrive. During the stream the buffer is shown as **raw text**; once generation completes, the final markdown is **rendered once** as the formatted document (§8.3). Live-rendering half-formed markdown — a `##` header with no body yet, an unclosed `**` — flickers and looks broken, so rendering is deferred to completion while the raw stream still gives the clinician immediate feedback.

**Storage engine.** All clinical data is held in a single **SQLite database encrypted with SQLCipher**, so the note, its versions, and the transcript are encrypted at rest and never leave the device. Application settings remain in the separate plain JSON store (no PHI), as decided for Phase 1.

**Audio retention.** The audio recording is **discarded** once the transcript is finalized — it is never written to the database. This minimizes the PHI footprint: the most sensitive raw artifact does not persist. Re-transcription from audio is therefore not possible in v1, which is an accepted trade-off.

**Versioning.** A note **version** is a generation event:

- Each **Generate / Regenerate** produces a new immutable version. All versions are retained and the clinician can revert to any prior one (§8.1).
- **Manual edits** are **autosaved in place** on the current version — editing refines the active version rather than spawning a version per keystroke. A new version is created only by an explicit (re)generation.

**Data model (note generation).**

| Entity | Fields (indicative) | Notes |
|--------|---------------------|-------|
| `records` | id, label, language, created_at, transcript | One per recorded session; holds the finalized, editable transcript. No audio. |
| `notes` | id, record_id, soap_data, created_at, is_active | Many per record; one flagged active. Immutable except the active note's autosaved edits. |

(The full cross-feature data model and Tauri command/event contracts are consolidated in the Data Model & Interfaces section.)

### 8.6 Prompt-prefix caching (KV-cache reuse)

The one-shot prompt (§8.3) puts a large, **byte-identical prefix** in front of every generation: the system instruction plus the worked example. Only the transcript at the tail changes. 

**What is being cached.** When the model reads tokens it produces per-token intermediate state (the attention **KV cache**) that generation then reads from. The prefix's KV entries depend only on the prefix tokens, so once computed they are valid for *any* transcript that follows — provided the prefix tokens are unchanged and sit at the same positions. That is the whole reason §8.3 orders the prompt `[system + example] → [transcript] → [assistant]`: **only the run of tokens before the first differing token is reusable**, so the fixed part must come first.

1. **Prefill once, snapshot the state.** On load/warmup the engine tokenizes the fixed prefix (system + example + template scaffolding up to where the transcript begins), decodes it once into a context, then serializes the KV state **for that one sequence** into an in-memory byte buffer. It also records the **prefix token sequence**. The context is dropped. Using the *sequence-scoped* save (not the whole-context one) sizes the snapshot to the cells the prefix actually used (tens of MB), avoiding a transient ~1 GB allocation for the full N_CTX cache right after the model load — which would spike RAM exactly at the §7 residency threshold.
2. **Per note — restore, don't recompute.** Build a fresh context, load the snapshot into it (a memory copy of the prefix KV — no transformer math), then decode only the transcript tail at positions after the prefix and generate. The prefix is never re-prefilled.
3. **Reset is automatic.** The snapshot bytes are never mutated and each note uses its own throwaway context, so there is nothing to trim between notes — the next note simply restores the same snapshot again. A cancelled or errored note drops its context with no effect on the cache.

**Byte-identical to the fallback.** A tokenizer can merge tokens across the prefix/tail boundary, so `tokenize(prefix) ++ tokenize(tail)` is not always `tokenize(prefix + tail)`. To keep a cached note identical to an uncached one, each generation tokenizes the **whole** prompt (exactly as the fallback does) and only restores the snapshot when that full token sequence **begins with the saved prefix tokens**. If the boundary merged, the check fails and the note falls back to a full decode — correct, just uncached.

**Correctness invariants.**

- **Prefix must be truly fixed.** If anything in the prefix changes, the snapshot is stale and must be rebuilt. The triggers are: a **model change** (Settings switches tier → different tokenizer/template, §8.2) and any **prompt edit** (system prompt or example). On either, the snapshot is dropped with the model (`unload`) and re-primed on the next load. The saved-prefix-tokens check also prevents restoring one model's state under another.
- **No accumulation.** Because every note uses a fresh context, transcript+note tokens never persist across notes, so nothing can accumulate toward the context window (§8.2) — the property the persistent-context alternative would have needed an explicit KV trim to hold.
- **Single-flight.** Generation is already serialized behind the model lock (§8.4); the snapshot is guarded the same way, so two notes never share/mutate it concurrently.

**Interaction with residency (§7).** This helps **co-resident** mode only. In **swap** mode the model is unloaded after each generation to free RAM for STT (§8.4), which destroys the context and its cached prefix — so the next note re-prefills from cold regardless. That is acceptable: swap mode is the RAM-constrained path where keeping a model (and context) warm isn't affordable anyway. 

**Dependency & fallback.** Reuse needs the `llama-cpp-2` binding to expose **sequence-scoped state save/restore** (`state_seq_get_size_ext` / `state_seq_get_data_ext` / `state_seq_set_data_ext`), confirmed present in the pinned version (0.1.150). The sequence-scoped variants (over the whole-context `get_state_size` / `copy_state_data`) are what keep the snapshot sized to the prefix's cells rather than the full N_CTX cache. The binding *also* exposes KV-cache trim (`clear_kv_cache_seq`), which the persistent-context alternative would have used — but the save/restore path is what we build (see the trade-off below). The **fallback** is the current behavior — prefill the full prompt per note — and it is entered automatically whenever the snapshot is missing (not yet primed, or after `unload`) or the boundary check fails, so the feature degrades to "correct but not cached" rather than breaking generation. The same fixed-prefix lever is shared by the correction pass (§6.7), so both benefit from one implementation.

**Non-goals.** No cross-*session* (on-disk) cache — the prefix is cheap to prefill once per app run, and persisting KV state to disk adds complexity and a versioning/staleness surface for no material gain. No caching of the transcript or generated tokens (they are unique per note).

## 9. Data Model & Interfaces

This section consolidates what the app persists and the Tauri command/event contracts that cross the React ↔ Rust boundary. It gathers contracts introduced piecewise in §6 and §8 into one reference.

### 9.1 Storage

Two stores, split by sensitivity:

- **Clinical DB** — a single **SQLite** file encrypted with **SQLCipher** (§8.5). Holds all PHI: encounters, transcripts, and notes.
- **Settings store** — a separate **plain JSON** file, unencrypted. Holds only app configuration; **no PHI**, so it needs no encryption.

The SQLCipher key derivation/protection is covered in the Security & Compliance section.

### 9.2 Schema (clinical DB)

Two tables. One record has many notes (one row per Generate/Regenerate, §8.5). Both store PHI and live inside the SQLCipher DB, so the transcript and notes are encrypted at rest with no separate encryption step.

**`records`**

| Column | Type | Notes |
|--------|------|-------|
| `id` | TEXT (UUID) | Primary key |
| `label` | TEXT | Free-text title the doctor types; opaque, not parsed |
| `language` | TEXT | `en` or `fr` |
| `created_at` | INTEGER | Unix timestamp |
| `transcript` | TEXT | Finalized transcript, stored inline (autosaved incrementally for crash safety, NFR-8) |

**`notes`**

| Column | Type | Notes |
|--------|------|-------|
| `id` | TEXT (UUID) | Primary key |
| `record_id` | TEXT (UUID) | FK → `records.id` |
| `soap_data` | TEXT | SOAP-R note, markdown with the five `##` headers (§8.3) |
| `created_at` | INTEGER | Unix timestamp |
| `is_active` | INTEGER | 1 for the current note; exactly one active per record |

### 9.3 Settings store (JSON)

**`Doctor facing settings`**

| Key | Surfaced? | Meaning |
|-----|-----------|---------|
| `model_choice` | **Doctor** | `best` (Mistral-7B) / `medium` (Phi-3.5 Q8) / `okay` (Phi-3.5 Q4); options the machine can't run are greyed out (§7) |
| `mic_device` | **Doctor** | Selected input device |
| `residency_mode` | internal | Co-resident vs swap, decided once (§7) |

**`Internal settings`**

| Key | Surfaced? | Meaning |
|-----|-----------|---------|
| `observed_total_ram` | internal | Cached probe; re-probed only on hardware change (§7) |
| `vad_threshold` | internal | Fixed sensible default (§6.2) |
| `idle_timeout` | internal | Auto-stop-on-silence default |

### 9.4 Tauri commands (UI → backend `invoke`)

The backend owns all state; commands are requests, and state guards reject illegal transitions (§6.6).

| Group | Commands | Effect |
|-------|----------|--------|
| Recording | `start_recording`, `stop_recording`, `pause_recording`, `resume_recording` | Drive the IDLE→RECORDING→PROCESSING state machine (§6.6, FR-4) |
| Transcript | `update_transcript` | Save the doctor's edits |
| Notes | `generate_note`, `regenerate_note`, `cancel_generation`, `update_note`, `revert_version` | Produce/edit/cancel notes; flip the active version (§8.4–8.5) |
| Records | `list_records`, `open_record`, `delete_record` | Saved-encounter browsing (FR-13); `delete_record` is permanent (NFR-9) |
| Settings | `get_settings`, `update_settings` | Read/patch the JSON store, including mic device |
| Hand-off | `copy_to_clipboard` | Copy a SOAP section's plain text to the clipboard for manual paste into the EMR. `paste_section` (one-key hotkey paste) is built but dormant, reserved for the deferred overlay (§13) |

### 9.5 Tauri events (backend → UI `emit`)

| Event | Payload | When |
|-------|---------|------|
| `transcript-segment` | `{ seq, text }` | Each transcribed segment; UI appends (§6) |
| `input-level` | `{ level }` | Live mic-level meter (FR-12) |
| `generation-token` | `{ text }` | Streaming note tokens during GENERATING (§8.5) |
| `state-changed` | `{ state }` | IDLE / RECORDING / PROCESSING / GENERATING transitions; GENERATING→IDLE signals the note is done and the UI loads the active note |
| `error` | `{ code, message }` | Recoverable failures (e.g. RAM guard trips, §8.4) |

## 10. Security & Compliance

The product's core promise is that patient data stays on the device, encrypted, and never leaks. This section records how the key is protected, how access is controlled, and the compliance posture under Canadian law (PHIPA/PIPEDA).

### 10.1 Encryption at rest & key management

The clinical DB is encrypted with SQLCipher (AES-256, §9.1). The open question was where to keep the encryption key, since it must live on the same device as the data it unlocks.

**Choice: Windows DPAPI, no passphrase.** On first run the app generates a random AES-256 key, then hands it to **Windows DPAPI** (`CryptProtectData`) scoped to the logged-in Windows user. DPAPI returns a wrapped (encrypted) blob, which is all that's stored on disk; the raw key is never persisted in readable form. On launch the app calls DPAPI to unwrap the key and opens the DB with it — no password prompt.

- **Frictionless:** the doctor logs into Windows as usual.
- **Bound to the account:** the wrapped key is meaningless on another Windows account or machine, so a stolen laptop's DB file is unreadable.
- **Trade-off (backup caveat):** because the key is tied to the Windows user account, losing that account (OS reinstall, profile wipe) makes existing encrypted data unrecoverable. Device/account backup is the clinic's responsibility.

### 10.2 Data residency

**Zero PHI egress (NFR-6).** The app is fully functional offline and makes no network calls that carry patient data. Transcripts, notes, and the patient label never leave the device.

### 10.3 Telemetry

**Automatic technical telemetry (no PHI).** To know the product works on real devices and to fix it early, the app sends **technical-only** events automatically — no opt-in, disclosed once in a short privacy notice. This preserves NFR-6, which concerns *PHI* egress: these events carry none. It covers both crashes and a small set of usage/health events.

- **Allowlist, never blocklist.** Each event is built from a fixed set of non-PHI fields — app version, OS, arch, detected RAM tier, active model tier, event name, coarse timings, and for errors the error *type/message string only* (`TechnicalContext`). Nothing else is attachable by construction.
- **Scrub backstop (defense-in-depth).** Every outgoing event still passes through `scrub_event`, which recursively strips any field whose key looks like PHI (`transcript`, `soap`, `note`, `label`, `record`) — so a future richer payload can never leak one.
- **Events:** `app_launched`, `setup_started` / `model_download_done` / `model_download_failed`, `note_generated` (+ ms), `correction_pass_ran`, `error` (sanitized), `crash` (stack trace + `TechnicalContext`).
- **Transport:** the **Sentry Rust SDK** sends each event to **our own self-hosted GlitchTip** instance (§14.4) — the SDK queues, batches, and retries in a background thread, never blocks the UI, and is silent on failure. Every event still passes the `scrub_event` backstop via the SDK's `before_send` hook before it leaves the process. It is off unless **both** the `crash-reporting` cargo feature is enabled **and** a DSN is compiled into the build (`MEDSCRIBE_CRASH_DSN`), so the default build has no client, sends nothing, and stays fully offline. GlitchTip is Sentry-API-compatible and self-hosted, so no third-party analytics vendor receives any data.

### 10.4 Data lifecycle

- **Audio:** discarded immediately after each segment is transcribed; never persisted (NFR-9, §8.5).
- **Transcripts & notes:** retained encrypted until the doctor deletes them; **deletion is permanent** — no recycle bin, no cloud copy (NFR-9).
- **Clipboard:** EMR hand-off places a section on the system clipboard, which is **auto-cleared a few seconds after paste**, limiting how long PHI lingers in a shared buffer.

### 10.5 Compliance posture (PHIPA/PIPEDA)

The clinic/clinician is the **custodian** of the health information; the app is the tool they use. The design supports their obligations: data minimization (no audio retention, opaque label, no extra patient metadata), encryption at rest (§10.1), and no third-party disclosure (zero PHI egress). **No audit log in v1** — with one clinician per device there is no separate party to audit; per-access logging is deferred to Future Considerations should a multi-user clinic ever require a trail.

## 11. Trade-offs & Alternatives

### Prompt-prefix caching: state save/restore vs a persistent context (§8.6)

- **Decision:** How to reuse the fixed prompt prefix's KV so it isn't re-prefilled on every note.
- **Options:**
  1. **Persistent context + KV trim** — hold one long-lived `LlamaContext` in the engine, prefill the prefix into it once, append each transcript, and trim the KV cache back to the prefix boundary between notes. This is what §8.6 originally planned.
  2. **State save/restore into a fresh context** — prefill the prefix once, snapshot its KV state to an in-memory buffer, and restore that snapshot into a fresh throwaway context per note before decoding the transcript tail.
- **Chosen:** Option 2 (save/restore).
- **Rationale:** In `llama-cpp-2`, a `LlamaContext` borrows the loaded `LlamaModel`, so storing a persistent context beside the owned model in the engine struct is **self-referential** — safe Rust won't allow it without `unsafe`/lifetime hacks or an extra self-referencing crate. Both options skip the same expensive step (running the prefix through every model layer — seconds of CPU), which is the entire point of the feature. Option 2 reuses the fresh-context-per-note path the engine already had, so it needs no new lifetime machinery; it also makes reset-to-boundary and cancel/error cleanup **automatic** (the snapshot is never mutated and each note's context is discarded), removing the mandatory-trim invariant Option 1 carries. Its only extra cost is a per-note memory copy of the snapshot — **milliseconds against the seconds saved** — so the win is effectively identical while the code stays simpler and safer. The binding exposes both APIs (0.1.150), so this is a design choice, not a capability limit.
- **Revisit if:** profiling on Windows shows the per-note snapshot restore is a meaningful share of latency (it shouldn't be), or the prefix grows large enough (e.g. few-shot, §8.3) that the snapshot's memory/copy cost matters — then reconsider a persistent context via a self-referential wrapper.

### Correction suggestions: review list vs inline anchored popups (§6.7)

- **Decision:** How to present streamed post-ASR correction suggestions over the editable transcript.
- **Options:**
  1. **Inline anchored popups** — anchor each suggestion as a non-blocking popup next to its flagged phrase, exactly where the mishearing sits in the text. This is what §6.7 originally described.
  2. **Review list beside the transcript** — show the suggestions as a list next to the transcript, each row `original → replacement` with Accept / Reject; Accept patches the span in place.
- **Chosen:** Option 2 (review list).
- **Rationale:** The transcript editor is a plain `<textarea>`. A textarea cannot host overlay elements at arbitrary character offsets — Option 1 would require replacing it with a mirror-overlay or `contentEditable` surface and tracking each suggestion's character offset as the clinician edits the text underneath it (the offsets shift on every insertion/deletion, and a stale offset anchors the popup to the wrong span). That offset-bookkeeping is exactly the fragility flagged as the phase's open risk. The review list carries **no offsets**: a suggestion is `{original, replacement}`, and Accept simply replaces the first occurrence of `original` in the current transcript — correct regardless of intervening edits, and if the span is gone the suggestion is silently dropped. Both options are equally *suggest-only* and non-blocking (the safety property is unchanged); the list trades literal spatial adjacency for robustness and a far smaller, testable implementation. The streamed, parse-as-you-go delivery (§6.7) is identical either way — only the anchoring differs.
- **Revisit if:** clinicians find the list hard to correlate with the phrase in a long transcript — then add lightweight highlighting (scroll-to / transient mark on hover) or move to a rich-text editor where true inline anchoring, with proper offset-mapping, becomes affordable.

### Beta trial gate: compiled-in expiry vs a server-enforced license

- **Decision:** How to enforce a fixed end date on the time-limited beta, after which the app stops working.
- **Options:**
  1. **Compiled-in expiry** — bake the trial end date into the binary as a constant; on launch, compare the system clock to it and block the app once past it. No account, no network.
  2. **Server-enforced license** — put signup/login (e.g. Supabase Auth) in front of the app, mint a per-user ID, and check the trial server-side, caching a session for offline grace.
- **Chosen:** Option 1 (compiled-in expiry).
- **Rationale:** The beta is a handful of **trusted** clinicians for roughly a month, so the threat model is "make the honest stop honest," not "defeat a determined attacker." Option 1 needs no backend, no login screen, and — critically — keeps the app **fully offline**, preserving the core no-PHI-egress / no-network posture (NFR-6); Option 2 would force at least one online authentication, turning the app network-dependent for a purely administrative gate. The date is evaluated in Rust and enforced in **two** places so the UI isn't the only guard: the frontend shows an expired screen before anything renders, and the work-initiating backend commands (`start_recording`, note generation, correction) reject once expired, so driving the IPC bridge directly is also blocked. The accepted weakness is **local-clock rollback** — a user could set their PC date back — which is unfixable without the very server Option 1 avoids; acceptable for trusted testers and consistent with the deferred code-signing risk posture.
- **Revisit if:** the app moves beyond a trusted beta to a paid or wider release where clock-rollback or license-sharing actually matters — then a server-enforced license (Option 2), naturally paired with the account system that a commercial launch needs anyway, becomes worth its cost.

## 12. Pricing

The economics are a direct consequence of the on-device design: there is **no marginal cost per encounter**. Everything runs locally on the clinician's existing laptop using open-source models that are free for commercial use (NFR-15) — no cloud API fees, no cloud compute, no cloud storage, no per-seat usage metering.

### 12.1 Cost to the clinic

**No recurring cost.** Unlike cloud scribe products that charge per-seat monthly subscriptions, this app incurs no ongoing fee for the clinic. Compute, storage, and inference all happen on hardware the clinic already owns. This is the core "no subscription / cost-effective" value proposition.

The natural fit is a **one-time purchase or flat per-device license**.

### 12.2 Vendor-side costs

The vendor carries a small, **fixed** operating cost — independent of how many encounters are processed:

| Cost | Nature | Notes |
|------|--------|-------|
| Crash-reporting service | Low monthly | Receives the scrubbed, PHI-free crash reports (§10.3); a hosted service (e.g. Sentry-class). Scales with crash volume, not patient volume |
| Windows code-signing certificate | Annual | Keeps the installer trusted/unflagged on Windows |

## 13. Future Considerations

Items deliberately deferred from v1, to revisit once the core product is validated.

| Item | What it adds | Why deferred from v1 |
|------|--------------|----------------------|
| **EMR integration** | Direct integration with the EMR (field auto-mapping or an EMR API) instead of manual paste | v1 has no EMR API integration; the manual hand-off is reliable and EMR-agnostic |
| **Fine-tuned models** | Note model fine-tuned on SOAP datasets for more consistent output | Few-shot prompting (§8.3) is a cheaper, reversible lever; no evidence yet that fine-tuning is needed |
| **AI engineering for larger context** | Context-handling techniques (e.g. chunking, summarization, retrieval) for transcripts that exceed the model window | The model window far exceeds a realistic consult (§8.3), so the whole transcript fits in one prompt today; needed only for much longer inputs |
| **Selectable alternate STT engine** | A user-selectable higher-accuracy / weaker-hardware STT option (e.g. a Whisper-family model) alongside the default Parakeet engine | A native-build constraint, not a product objection — see the note below. Parakeet (§6.4) covers EN+FR well, so a second engine is a refinement, not a v1 need |

The r2.dev base URL should later swap to a custom domain (production polish, tied to #10).

**Why the alternate STT engine is deferred (technical note).** The default STT engine (Parakeet) runs on an **ONNX** runtime, while the note-generation LLM (§8) runs on **llama.cpp**. A Whisper-family STT engine would run on **whisper.cpp**. Both whisper.cpp and llama.cpp statically embed their *own* copy of the same low-level tensor library (**ggml**); linking both into one executable produces duplicate-symbol link errors, so they cannot coexist in a single binary. v1 therefore ships exactly one ggml consumer — the LLM — and an ONNX-based STT (Parakeet) that carries no ggml, which links cleanly.

Adding a whisper-based engine later is still possible without this conflict by running one engine **out-of-process** (a separate child process the app talks to locally), so each binary embeds its own ggml independently. That isolation pairs naturally with the **swap** residency mode (§7) — the alternate engine and the LLM would load one at a time, with the clinician shown a brief, plain-language "this may add a short delay" notice at the hand-off rather than any technical detail. This is a known, accepted limitation of the current single-binary design.

## 14. Distribution, Updates & Telemetry

How the app reaches users, how they get updates, and where the technical telemetry (§10.3) is stored. All three are deliberately lightweight and self-owned — no app store, no third-party analytics vendor.

### 14.1 Installer & hosting

- **Installer:** `bun run tauri build` on Windows produces an **NSIS `.exe`** (and MSI) via Tauri's bundler. The build-time toolchain (LLVM/Clang, CMake, Perl) compiles llama.cpp into the native binary and is **not** shipped; end users need none of it. The only runtime dependency is **WebView2**, which ships with Windows 10/11 and the NSIS installer auto-installs if missing.
- **Lean installer.** The installer carries only the app plus the small Silero VAD (`silero_vad.onnx`, ~1.8 MB); the LLM and Parakeet STT are fetched on **first launch** (§8.2, Phase D3), keeping the `.exe` at tens of MB rather than several GB. `bundle.resources` therefore bundles only the VAD.
- **Host: GitHub Releases.** Each version is a tagged GitHub Release with the `.exe` attached as an asset. The 2 GB-per-asset limit is irrelevant here because models are **not** hosted on GitHub — they download from their existing source (Hugging Face) on first run, and that URL is a **swappable constant** (movable to our own R2/S3 later without breaking already-installed clients, since they only re-download on a fresh install).

### 14.2 Download website

- A static page on **Vercel** with a "Download for Windows" button linking to the release asset via the **`/releases/latest/download/<asset>`** redirect, so the link never goes stale across versions.
- Telemetry ingest is **not** hosted here — it lands on a separate self-hosted GlitchTip instance (§14.4).

### 14.3 Updates

Two paths ship: an automatic in-app updater (default), with manual re-install always available as the fallback.

**Auto-update (Tauri updater plugin).**
- `createUpdaterArtifacts: true` — the build emits a **signed** update bundle alongside the installer. Updates are signed with a keypair (`tauri signer generate`); the **public key** lives in `tauri.conf.json`, the **private key** is a CI/release secret. The app installs an update only if its signature verifies against the embedded public key, so a tampered or spoofed bundle is rejected.
- **Manifest + host.** Each release publishes a `latest.json` (version, notes, per-target signed-bundle URL). Both the manifest and the bundle are **GitHub Release assets**, so the update host is the same as the download host (§14.1) — no extra infrastructure. The updater's `endpoints` point at the release URL.
- **Check + apply.** On launch the app makes **one** best-effort HTTPS call to the manifest endpoint. If a newer version exists it shows a **non-blocking prompt** ("Update available — install and restart?"); the clinician chooses when, so an update never interrupts a consult. Offline or endpoint-unreachable → the check fails silently and the app runs normally.
- **PHI posture.** The update check carries **no PHI** — it sends only the current version (in the request URL) and receives the manifest. It is the one outbound call besides telemetry (§10.3), and like telemetry it is best-effort and silent on failure, so the offline default still holds.

**Manual fallback.** A new version is always also a plain GitHub Release; a user can download and run the new installer (which upgrades in place) if they prefer, or if auto-update is ever disabled. The download site (§14.2) always points at latest.

### 14.4 Telemetry backend

The client-side policy (what is collected, the allowlist, the scrub backstop) is §10.3; this is where events land.

- **Backend: self-hosted GlitchTip.** GlitchTip is an open-source, Sentry-API-compatible error-tracking server we run ourselves (its own container + Postgres). Because it speaks the Sentry protocol, the app uses the stock **Sentry Rust SDK** as the client — no custom ingest endpoint, no bespoke schema. Crashes, `error` events, doctor feedback, and the named product events (`app_launched`, `note_generated`, `correction_pass_ran`, …) all arrive as Sentry events; `TechnicalContext` and per-event `props` ride along as event extras/tags, and error/crash reports carry the stack trace.
- **DSN, not a public POST URL.** The app is configured with a **DSN** (`MEDSCRIBE_CRASH_DSN`), a send-only client ingest key that names the GlitchTip project and authenticates submissions. It is baked in at compile time (`option_env!`) and safe to ship in the binary. GlitchTip's own per-project rate-limiting and DSN scoping guard the endpoint — there is no separate shared-secret header to manage.
- **`install_id`** is a random UUID generated once per install and stored in the settings file, attached to events as the Sentry user/device id. It identifies a *device*, never a patient, so distinct-device counts and per-device error rates are possible without any PHI or personal identifier.
- The whole path — GlitchTip server and its database — is infrastructure we own and host; only the Sentry *SDK* (a client library speaking an open protocol) is embedded in the app, and no third-party analytics vendor receives any data.

### 14.5 Release pipeline & versioning

- **CI/CD (GitHub Actions).** On every push/PR, CI runs `cargo test` + `bun test` + a compile (catch breakage early). On a **version tag**, CD spins up a Windows runner → `tauri build` → code-sign → publish the GitHub Release with the installer, the signed updater bundle, and `latest.json` (§14.3). This removes the manual, error-prone per-release steps and guarantees every release is signed and complete.
- **Version-bump discipline.** The auto-updater decides "is there an update?" by comparing the installed `tauri.conf.json` version against the released one. **The version must be bumped before every release** or the updater sees no change and users never receive it. CD enforces this by tagging = the source of the version, so a release can't be cut without a new number.
