// Acceptance fixture: optional chaining, nullish coalescing and global fetch
// all require a modern Node.js runtime.
const payload = { user: { profile: null } };

const name = payload.user?.profile?.name ?? "acceptance";

async function main() {
  const response = await fetch("https://example.invalid/ping").catch(() => null);
  console.log(`node-service says hello to ${name}: ${response ? response.status : "offline"}`);
}

main();
