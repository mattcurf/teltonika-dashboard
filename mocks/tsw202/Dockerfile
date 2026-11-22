# Mock TSW202. Build from the mocks/ directory: docker build -f tsw202/Dockerfile mocks
FROM node:25-alpine
WORKDIR /app
# The real device serves HTTPS with a self-signed certificate; so does the mock.
RUN apk add --no-cache openssl \
  && mkdir tls \
  && openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=TSW202" -keyout tls/key.pem -out tls/cert.pem \
  && apk del openssl
COPY package.json ./
COPY common ./common
COPY tsw202 ./tsw202
EXPOSE 443
HEALTHCHECK --interval=3s --timeout=3s --retries=20 \
  CMD node -e "require('https').get({host:'127.0.0.1',path:'/api/health',rejectUnauthorized:false},()=>process.exit(0)).on('error',()=>process.exit(1))"
CMD ["node", "tsw202/server.ts"]
