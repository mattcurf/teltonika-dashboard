# The panel. It has no runtime dependencies: Node runs the TypeScript sources directly.
FROM node:25-alpine
WORKDIR /app
COPY package.json ./
COPY src ./src
COPY public ./public
ENV PORT=8080
EXPOSE 8080
HEALTHCHECK --interval=3s --timeout=5s --retries=20 \
  CMD node -e "fetch('http://127.0.0.1:8080/api/status').then(r=>process.exit(r.ok?0:1),()=>process.exit(1))"
CMD ["node", "src/server.ts"]
