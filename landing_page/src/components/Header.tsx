import { useState } from 'react'
import { Link, NavLink } from 'react-router-dom'
import Logo from './Logo'

const navLinkClass = ({ isActive }: { isActive: boolean }) =>
  `text-sm font-medium transition-colors ${
    isActive ? 'text-white' : 'text-slate-300 hover:text-white'
  }`

export default function Header() {
  const [open, setOpen] = useState(false)

  return (
    <header className="sticky top-0 z-50 border-b border-white/5 bg-ink-950/80 backdrop-blur">
      <div className="mx-auto flex max-w-6xl items-center justify-between px-5 py-3.5">
        <Link to="/" onClick={() => setOpen(false)}>
          <Logo />
        </Link>

        <nav className="hidden items-center gap-8 md:flex">
          <NavLink to="/" end className={navLinkClass}>
            Home
          </NavLink>
          <a
            href="/#features"
            className="text-sm font-medium text-slate-300 transition-colors hover:text-white"
          >
            Features
          </a>
          <a
            href="/#requirements"
            className="text-sm font-medium text-slate-300 transition-colors hover:text-white"
          >
            Requirements
          </a>
          <Link
            to="/download"
            className="rounded-lg bg-brand-500 px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-brand-400"
          >
            Download
          </Link>
        </nav>

        <button
          className="inline-flex h-10 w-10 items-center justify-center rounded-lg border border-white/10 text-slate-200 md:hidden"
          aria-label="Toggle menu"
          aria-expanded={open}
          onClick={() => setOpen((v) => !v)}
        >
          <span className="text-lg leading-none">{open ? '✕' : '☰'}</span>
        </button>
      </div>

      {open && (
        <nav className="flex flex-col gap-1 border-t border-white/5 px-5 pb-4 pt-2 md:hidden">
          <NavLink to="/" end onClick={() => setOpen(false)} className="py-2 text-slate-200">
            Home
          </NavLink>
          <a href="/#features" onClick={() => setOpen(false)} className="py-2 text-slate-200">
            Features
          </a>
          <a href="/#requirements" onClick={() => setOpen(false)} className="py-2 text-slate-200">
            Requirements
          </a>
          <Link
            to="/download"
            onClick={() => setOpen(false)}
            className="mt-1 rounded-lg bg-brand-500 px-4 py-2 text-center font-semibold text-white"
          >
            Download
          </Link>
        </nav>
      )}
    </header>
  )
}
