#!/bin/sh
set -eu

if ! command -v brew >/dev/null 2>&1; then
  echo "Homebrew no está instalado. Visita https://brew.sh y vuelve a ejecutar este script."
  exit 1
fi

echo "Instalando herramientas locales de video y metadatos..."
brew install ffmpeg exiftool imagemagick

if ! command -v node >/dev/null 2>&1; then
  echo "Instalando Node.js para compilar la interfaz..."
  brew install node
fi

if ! xcode-select -p >/dev/null 2>&1; then
  echo "Faltan las herramientas de compilación de Apple."
  echo "Ejecuta: xcode-select --install"
  exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust no está instalado. Instálalo desde https://rustup.rs y vuelve a ejecutar este script."
  exit 1
fi

echo "Instalando dependencias de la aplicación..."
npm install

echo "Listo. Inicia Casi Cine con: npm run dev"
echo "Consulta README.md para instalar Ollama, Qwen, Whisper o ACE-Step de forma explícita."
