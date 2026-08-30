/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** Lambda Function URL origin, e.g. https://abc123.lambda-url.ca-central-1.on.aws */
  readonly VITE_API_BASE_URL: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
