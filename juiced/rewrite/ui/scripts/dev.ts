import index from "../index.html";

const upstream = new URL(process.env.JUICED_DEV_API ?? "http://127.0.0.1:13306");
const server = Bun.serve({
  hostname: "127.0.0.1",
  port: Number(process.env.PORT ?? 5178),
  development: { hmr: true, console: true },
  routes: {
    "/": index,
    "/v1/*": (request) => {
      const target = new URL(new URL(request.url).pathname + new URL(request.url).search, upstream);
      return fetch(target, {
        method: request.method,
        headers: request.headers,
        body: request.body,
        signal: request.signal,
        redirect: "manual",
      });
    },
  },
});
console.log(`Juiced console ${server.url} → ${upstream.origin}`);
