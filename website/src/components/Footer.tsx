import { Link } from 'react-router-dom'
import Logo from './Logo'

export default function Footer() {
  return (
    <footer className="border-t border-white/5 bg-ink-950">
      <div className="mx-auto flex max-w-6xl flex-col items-center justify-between gap-6 px-5 py-8 sm:flex-row">
        <Logo className="h-8 w-8" />
        <nav className="flex items-center gap-6 text-sm text-slate-400">
          <Link to="/" className="transition-colors hover:text-white">
            Home
          </Link>
          <Link to="/download" className="transition-colors hover:text-white">
            Download
          </Link>
        </nav>
        <p className="text-sm text-slate-500">
          © {new Date().getFullYear()} ASmart Medical Scribe
        </p>
      </div>
    </footer>
  )
}
