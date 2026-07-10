/** @type {import('tailwindcss').Config} */
export default {
  content: ['./index.html', './src/**/*.{ts,tsx}'],
  theme: {
    extend: {
      colors: {
        brand: {
          DEFAULT: '#0d8a86',
          50: '#eafaf8',
          100: '#c8f0ec',
          200: '#94e1db',
          300: '#5accc4',
          400: '#2bb0a8',
          500: '#0d8a86',
          600: '#0b756f',
          700: '#0d5d59',
          800: '#0f4b48',
          900: '#0f3e3c',
        },
        ink: {
          950: '#070b12',
          900: '#0b1220',
          800: '#111a2b',
          700: '#1a2740',
        },
      },
      fontFamily: {
        sans: ['Inter', 'system-ui', '-apple-system', 'Segoe UI', 'Roboto', 'sans-serif'],
      },
    },
  },
  plugins: [],
}
