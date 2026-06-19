import { readDashboardConfig } from "@/src/config";

const tabs = ["Overview", "Projects", "Quota", "Defects"];

export default function DashboardShell() {
  const config = readDashboardConfig();

  return (
    <main className="mx-auto flex min-h-screen w-full max-w-6xl flex-col gap-8 px-6 py-8 text-ink md:px-10">
      <header className="rounded-3xl border border-white/70 bg-white/80 p-8 shadow-sm backdrop-blur">
        <p className="text-sm font-semibold uppercase tracking-[0.3em] text-indigo-600">SYM-96 foundation</p>
        <div className="mt-4 flex flex-col gap-4 md:flex-row md:items-end md:justify-between">
          <div>
            <h1 className="text-4xl font-semibold tracking-tight md:text-5xl">Symphony Dashboard</h1>
            <p className="mt-3 max-w-2xl text-base leading-7 text-slate-600">
              Minimal dashboard shell with Rust API, BFF, quota, and live-update foundations wired for future surfaces.
            </p>
          </div>
          <div className="rounded-2xl bg-slate-950 px-5 py-4 text-sm text-white shadow-sm">
            <span className="block text-slate-400">Rust API</span>
            <span className="font-mono">{config.symphonyApiBase}</span>
          </div>
        </div>
      </header>

      <nav aria-label="Dashboard sections" className="grid grid-cols-2 gap-3 md:grid-cols-4">
        {tabs.map((tab) => (
          <button
            key={tab}
            className="rounded-2xl border border-slate-200 bg-white/85 px-4 py-4 text-left text-sm font-semibold shadow-sm transition hover:-translate-y-0.5 hover:border-indigo-300 hover:shadow-md focus:outline-none focus:ring-2 focus:ring-indigo-500"
            type="button"
          >
            {tab}
            <span className="mt-2 block text-xs font-medium text-slate-500">Shell state ready</span>
          </button>
        ))}
      </nav>

      <section className="grid gap-4 md:grid-cols-3">
        <StatusCard title="BFF routes" value="Ready" detail="Dashboard, project, issue, events, and quota endpoints are defined." />
        <StatusCard title="Refresh" value={`${config.refreshMs} ms`} detail={config.sseEnabled ? "SSE preferred with polling fallback." : "Polling fallback configured."} />
        <StatusCard title="Quota command" value="Configured" detail={config.ocuCommand} />
      </section>
    </main>
  );
}

function StatusCard({ title, value, detail }: { title: string; value: string; detail: string }) {
  return (
    <article className="rounded-3xl border border-white/70 bg-white/80 p-6 shadow-sm backdrop-blur">
      <h2 className="text-sm font-semibold uppercase tracking-[0.22em] text-slate-500">{title}</h2>
      <p className="mt-4 text-2xl font-semibold">{value}</p>
      <p className="mt-3 text-sm leading-6 text-slate-600">{detail}</p>
    </article>
  );
}
