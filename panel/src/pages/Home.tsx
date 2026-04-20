export default function Home() {
  return (
    <section className="space-y-6">
      <div>
        <h2 className="text-2xl font-semibold">Welcome</h2>
        <p className="text-white/60">
          Protocol-agnostic, local-first home automation. This is the Command Central surface.
        </p>
      </div>
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-4">
        <Tile title="Devices" body="Inventory and live state." />
        <Tile title="Scenes" body="One-tap room and household actions." />
        <Tile title="Alerts" body="System and automation notifications." />
      </div>
    </section>
  );
}

function Tile({ title, body }: { title: string; body: string }) {
  return (
    <div className="rounded-xl bg-white/5 hover:bg-white/10 border border-white/10 p-5 transition">
      <h3 className="text-lg font-medium">{title}</h3>
      <p className="text-sm text-white/60 mt-1">{body}</p>
    </div>
  );
}
