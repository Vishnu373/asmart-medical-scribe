import { Link } from 'react-router-dom'
import { REQUIREMENTS } from '../constants'

const features = [
  {
    icon: '🎙️',
    title: 'On-device transcription',
    body: 'Speech is converted to text right on the desktop. Audio is never uploaded anywhere.',
  },
  {
    icon: '📝',
    title: 'Automatic note drafting',
    body: 'The transcript is turned into a structured clinical note, ready for you to review and edit.',
  },
  {
    icon: '🔒',
    title: 'Private by architecture',
    body: 'There is no server to breach, because there is no server. Patient data never leaves the machine.',
  },
  {
    icon: '🖥️',
    title: 'Runs on standard PCs',
    body: 'CPU-only. No GPU, no dedicated hardware, and no special IT setup required.',
  },
  {
    icon: '📶',
    title: 'Works offline',
    body: 'Once installed, it runs without an internet connection — reliable in any exam room.',
  },
  {
    icon: '✏️',
    title: 'You stay in control',
    body: 'Generated notes are drafts for a clinician to review, edit, and sign off.',
  },
]

const steps = [
  {
    n: '1',
    title: 'Record the visit',
    body: 'Open ASmart Medical Scribe on your desktop and capture the conversation.',
  },
  {
    n: '2',
    title: 'Transcribe on-device',
    body: 'The audio is transcribed locally on your machine — nothing is sent to the cloud.',
  },
  {
    n: '3',
    title: 'Generate the note',
    body: 'A structured clinical note is drafted from the transcript, ready for your review.',
  },
]

export default function Home() {
  return (
    <>
      {/* Hero */}
      <section className="relative overflow-hidden">
        <div className="pointer-events-none absolute inset-0 bg-[radial-gradient(60%_50%_at_50%_0%,rgba(13,138,134,0.22),transparent_70%)]" />
        <div className="relative mx-auto max-w-6xl px-5 pb-16 pt-20 text-center sm:pt-28">
          <span className="inline-flex items-center gap-2 rounded-full border border-brand-500/30 bg-brand-500/10 px-3 py-1 text-xs font-semibold uppercase tracking-wide text-brand-300">
            <span className="h-1.5 w-1.5 rounded-full bg-brand-400" />
            Now in private beta
          </span>

          <h1 className="mx-auto mt-6 max-w-3xl text-4xl font-extrabold leading-tight tracking-tight text-white sm:text-6xl">
            Clinical notes that never leave your desktop.
          </h1>

          <p className="mx-auto mt-6 max-w-2xl text-lg leading-relaxed text-slate-300">
            ASmart Medical Scribe records the visit, transcribes the conversation, and drafts a
            structured clinical note — <span className="text-white">entirely on your own Windows
            device</span>. No cloud. No uploads. No patient data leaving the room.
          </p>

          <div className="mt-9 flex flex-col items-center justify-center gap-3 sm:flex-row">
            <Link
              to="/download"
              className="w-full rounded-xl bg-brand-500 px-7 py-3.5 text-base font-semibold text-white transition-colors hover:bg-brand-400 sm:w-auto"
            >
              Download for Windows
            </Link>
            <a
              href="#how"
              className="w-full rounded-xl border border-white/15 px-7 py-3.5 text-base font-semibold text-slate-200 transition-colors hover:border-white/30 hover:text-white sm:w-auto"
            >
              See how it works
            </a>
          </div>

          <ul className="mt-8 flex flex-wrap items-center justify-center gap-x-6 gap-y-2 text-sm text-slate-400">
            <li>100% on-device processing</li>
            <li className="hidden sm:block">·</li>
            <li>Works offline</li>
            <li className="hidden sm:block">·</li>
            <li>Windows 11 · CPU-only</li>
          </ul>

          {/* Screenshot placeholder — replace with a real app screenshot when ready */}
          <div className="mx-auto mt-14 max-w-4xl">
            <div className="flex aspect-[16/9] w-full items-center justify-center rounded-2xl border border-dashed border-white/15 bg-ink-800/50 text-sm text-slate-500">
              App screenshot goes here
            </div>
          </div>
        </div>
      </section>

      {/* Features */}
      <section id="features" className="mx-auto max-w-6xl px-5 py-20">
        <div className="mx-auto max-w-2xl text-center">
          <h2 className="text-3xl font-bold tracking-tight text-white sm:text-4xl">
            Built to do one job well
          </h2>
          <p className="mt-4 text-slate-400">
            Capture the encounter, produce a usable note, and keep everything private.
          </p>
        </div>

        <div className="mt-12 grid gap-5 sm:grid-cols-2 lg:grid-cols-3">
          {features.map((f) => (
            <div
              key={f.title}
              className="rounded-2xl border border-white/5 bg-ink-900 p-6 transition-colors hover:border-brand-500/30"
            >
              <div className="mb-4 flex h-11 w-11 items-center justify-center rounded-xl bg-brand-500/10 text-xl">
                {f.icon}
              </div>
              <h3 className="text-lg font-semibold text-white">{f.title}</h3>
              <p className="mt-2 text-sm leading-relaxed text-slate-400">{f.body}</p>
            </div>
          ))}
        </div>
      </section>

      {/* How it works */}
      <section id="how" className="border-y border-white/5 bg-ink-900/40">
        <div className="mx-auto max-w-6xl px-5 py-20">
          <div className="mx-auto max-w-2xl text-center">
            <h2 className="text-3xl font-bold tracking-tight text-white sm:text-4xl">
              How it works
            </h2>
            <p className="mt-4 text-slate-400">Three steps — all on your device.</p>
          </div>

          <ol className="mt-12 grid gap-6 md:grid-cols-3">
            {steps.map((s) => (
              <li key={s.n} className="relative rounded-2xl border border-white/5 bg-ink-900 p-6">
                <span className="flex h-10 w-10 items-center justify-center rounded-full bg-brand-500 text-base font-bold text-white">
                  {s.n}
                </span>
                <h3 className="mt-5 text-lg font-semibold text-white">{s.title}</h3>
                <p className="mt-2 text-sm leading-relaxed text-slate-400">{s.body}</p>
              </li>
            ))}
          </ol>
        </div>
      </section>

      {/* Requirements */}
      <section id="requirements" className="mx-auto max-w-6xl px-5 py-20">
        <div className="mx-auto max-w-2xl text-center">
          <h2 className="text-3xl font-bold tracking-tight text-white sm:text-4xl">
            System requirements
          </h2>
          <p className="mt-4 text-slate-400">
            Available for Windows desktops during the beta.
          </p>
        </div>

        <div className="mt-12 grid gap-5 sm:grid-cols-3">
          {REQUIREMENTS.map((r) => (
            <div
              key={r.label}
              className="rounded-2xl border border-white/5 bg-ink-900 p-6 text-center"
            >
              <div className="text-xs font-semibold uppercase tracking-wide text-brand-300">
                {r.label}
              </div>
              <div className="mt-2 text-lg font-semibold text-white">{r.value}</div>
            </div>
          ))}
        </div>

        <div className="mt-12 text-center">
          <Link
            to="/download"
            className="inline-block rounded-xl bg-brand-500 px-7 py-3.5 text-base font-semibold text-white transition-colors hover:bg-brand-400"
          >
            Download for Windows
          </Link>
          <p className="mt-4 text-sm text-slate-500">
            macOS and additional platforms are on the roadmap.
          </p>
        </div>
      </section>
    </>
  )
}
