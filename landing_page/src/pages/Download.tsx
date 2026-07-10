import { DOWNLOAD_URL, APP_VERSION, REQUIREMENTS } from '../constants'

const installSteps = [
  'Download the installer using the button above.',
  'Open asmart-medical-scribe-0.1.0-x64-setup.exe from your Downloads folder.',
  'If Windows shows a SmartScreen prompt, choose “More info” → “Run anyway” (the app is new and not yet widely recognised).',
  'Follow the installer, then launch ASmart Medical Scribe from the Start menu.',
]

const privacyPoints = [
  {
    title: 'Nothing goes to the cloud',
    body: 'Transcription and note generation both run locally. Recordings and notes stay on the machine you control.',
  },
  {
    title: 'Works offline',
    body: 'No internet connection is needed once installed — dependable even in rooms with poor connectivity.',
  },
  {
    title: 'CPU-only',
    body: 'Runs on standard Windows PCs with no GPU or special hardware.',
  },
]

export default function Download() {
  return (
    <div className="mx-auto max-w-4xl px-5 py-16 sm:py-20">
      {/* Download hero */}
      <div className="text-center">
        <span className="inline-flex items-center gap-2 rounded-full border border-brand-500/30 bg-brand-500/10 px-3 py-1 text-xs font-semibold uppercase tracking-wide text-brand-300">
          <span className="h-1.5 w-1.5 rounded-full bg-brand-400" />
          Private beta · v{APP_VERSION}
        </span>
        <h1 className="mx-auto mt-6 max-w-2xl text-4xl font-extrabold tracking-tight text-white sm:text-5xl">
          Download ASmart Medical Scribe
        </h1>
        <p className="mx-auto mt-5 max-w-xl text-lg text-slate-300">
          The on-device medical scribe for Windows. Everything runs on your machine — private by
          design.
        </p>

        <a
          href={DOWNLOAD_URL}
          className="mt-9 inline-flex items-center gap-2.5 rounded-xl bg-brand-500 px-8 py-4 text-lg font-semibold text-white transition-colors hover:bg-brand-400"
        >
          <span aria-hidden="true">⬇</span>
          Download for Windows (64-bit)
        </a>
        <p className="mt-3 text-sm text-slate-500">
          Windows 11 · installer (.exe) · v{APP_VERSION}
        </p>
      </div>

      {/* Requirements */}
      <section className="mt-16">
        <h2 className="text-xl font-bold text-white">System requirements</h2>
        <div className="mt-5 grid gap-4 sm:grid-cols-3">
          {REQUIREMENTS.map((r) => (
            <div key={r.label} className="rounded-xl border border-white/5 bg-ink-900 p-5">
              <div className="text-xs font-semibold uppercase tracking-wide text-brand-300">
                {r.label}
              </div>
              <div className="mt-1.5 font-semibold text-white">{r.value}</div>
            </div>
          ))}
        </div>
      </section>

      {/* Install steps */}
      <section className="mt-14">
        <h2 className="text-xl font-bold text-white">Installing</h2>
        <ol className="mt-5 space-y-4">
          {installSteps.map((step, i) => (
            <li key={i} className="flex gap-4">
              <span className="flex h-7 w-7 flex-none items-center justify-center rounded-full bg-brand-500/15 text-sm font-bold text-brand-300">
                {i + 1}
              </span>
              <span className="pt-0.5 leading-relaxed text-slate-300">{step}</span>
            </li>
          ))}
        </ol>
      </section>

      {/* Privacy recap */}
      <section className="mt-14 rounded-2xl border border-white/5 bg-ink-900/60 p-7">
        <h2 className="text-xl font-bold text-white">Why on-device</h2>
        <div className="mt-5 grid gap-6 sm:grid-cols-3">
          {privacyPoints.map((p) => (
            <div key={p.title}>
              <h3 className="font-semibold text-white">{p.title}</h3>
              <p className="mt-1.5 text-sm leading-relaxed text-slate-400">{p.body}</p>
            </div>
          ))}
        </div>
      </section>

      <p className="mt-10 text-center text-sm text-slate-500">
        ASmart Medical Scribe is in beta. Generated notes are AI drafts intended for clinician
        review and are not a substitute for professional judgment.
      </p>
    </div>
  )
}
