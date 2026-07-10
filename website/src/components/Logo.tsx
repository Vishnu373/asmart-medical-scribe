import logo from '../assets/ams_logo.png'

export default function Logo({ className = 'h-9 w-9' }: { className?: string }) {
  return (
    <span className="inline-flex items-center gap-2.5">
      <img
        src={logo}
        alt="ASmart Medical Scribe logo"
        className={`${className} rounded-lg`}
      />
      <span className="text-[15px] font-semibold tracking-tight text-white">
        ASmart <span className="text-brand-300">Medical Scribe</span>
      </span>
    </span>
  )
}
