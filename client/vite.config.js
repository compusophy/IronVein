import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [],
  server: {
    fs: {
      allow: ['..']
    }
  }
}); 