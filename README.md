# Casi Cine

**Tú vívelo. Casi Cine lo convierte en película.**

Casi Cine es una aplicación local para macOS que organiza fotos y videos familiares y crea una película cronológica con selección visual, diálogo, música y encuadre automático. Los originales se leen desde su ubicación y **nunca se modifican, renombran ni eliminan**.

Después de instalar las herramientas y modelos elegidos, el procesamiento funciona sin Internet. No hay cuentas, telemetría, subidas, APIs pagadas ni suscripciones.

## Qué funciona actualmente

- Importación recursiva de MOV, MP4, M4V, JPG, JPEG, PNG, HEIC y HEIF.
- Fechas originales mediante ExifTool y metadatos QuickTime, con fuentes inciertas marcadas explícitamente.
- Duplicados exactos, similitud perceptual, ráfagas y rechazo de fotografías repetitivas o irrelevantes.
- Análisis visual local con `qwen3.5:9b`: doce muestras a lo largo de cada video, clasificación de etapas del viaje y detección del acontecimiento completo.
- Transcripción local español/inglés con whisper.cpp para valorar conversaciones, risas y audio significativo. No se generan subtítulos en pantalla.
- Historia cronológica con cobertura del comienzo, trayecto, destinos, momentos familiares, naturaleza y regreso cuando el material los contiene.
- Selección de fragmentos con margen antes y después de la acción para conservar preparación, acción y reacción.
- Proyectos reabribles, caché SQLite, reanudación y regeneración sin repetir análisis terminado.
- Exportación H.264/AAC a 16:9, 9:16 o 1:1, con encuadre seguro y fondo desenfocado cuando recortar destruiría información.
- Normalización de audio, entrada/salida suave, música atenuada bajo diálogo y reintentos de render.
- Títulos de ubicación opcionales, obtenidos de coordenadas EXIF y una base geográfica incluida localmente.
- Música del usuario, modo sin música y generación local opcional con ACE-Step 1.5.
- JSON de la línea de tiempo junto al MP4.

## Instalación para usar la aplicación

### 1. Compatibilidad

- macOS 12 o posterior.
- Apple Silicon. El objetivo principal probado es M1 Pro con 16 GB de memoria.
- Aproximadamente 15–25 GB libres para herramientas, Qwen, cachés y proyectos. ACE-Step necesita unos 10 GB adicionales.

### 2. Instalar las herramientas esenciales

Instala [Homebrew](https://brew.sh/) si todavía no existe. Después abre Terminal y ejecuta:

```sh
brew install ffmpeg exiftool imagemagick
```

| Herramienta | Uso |
| --- | --- |
| FFmpeg / ffprobe | Leer, preparar, mezclar y renderizar video y audio |
| ExifTool | Fechas, orientación y coordenadas originales |
| ImageMagick | Crear títulos de ubicación con tipografía correcta |

Abre Casi Cine y pulsa **Revisar de nuevo** en la tarjeta de herramientas después de instalarlas.

### 3. Instalar la comprensión visual recomendada

Sin esta parte Casi Cine todavía puede ordenar y renderizar, pero la selección se basa principalmente en cronología y calidad técnica. Para que comprenda el contenido del viaje:

```sh
brew install ollama
brew services start ollama
ollama pull qwen3.5:9b
```

`qwen3.5:9b` ocupa aproximadamente 6.6–7.6 GB y acepta imágenes. Casi Cine lo carga localmente mediante Ollama y procesa storyboards pequeños de forma secuencial para mantenerse dentro de 16 GB de memoria.

### 4. Instalar transcripción local recomendada

```sh
brew install whisper-cpp
mkdir -p /opt/homebrew/share/whisper-cpp/models
curl -L --fail --output /opt/homebrew/share/whisper-cpp/models/ggml-small.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin
```

Casi Cine busca `whisper-cli` y, en Apple Silicon, los modelos:

- `/opt/homebrew/share/whisper-cpp/models/ggml-small.bin`
- `/opt/homebrew/share/whisper-cpp/models/ggml-base.bin`

La descarga solo es necesaria una vez. El modelo queda almacenado localmente.

### 5. Música generada — opcional

No hace falta ACE-Step para crear películas. Puedes elegir **Sin música** o añadir canciones locales. Para generar música original en esta Mac instala primero:

```sh
brew install git uv
```

Después usa **Instalar música local** dentro de Casi Cine. La aplicación muestra la descarga antes de iniciarla; instala ACE-Step 1.5, Python 3.12 aislado y aproximadamente 10 GB de modelos. La generación validada en M1 Pro de 16 GB usa MLX/MPS, lote uno y pistas cortas reutilizadas con crossfades.

ACE-Step se distribuye bajo MIT, pero ninguna pista generada está garantizada como libre de similitudes, reclamos de derechos o coincidencias de Content ID.

## Instalación automatizada desde el código fuente

El script instala las herramientas esenciales y las dependencias JavaScript:

```sh
chmod +x scripts/setup-macos.sh
./scripts/setup-macos.sh
```

Los modelos de IA no se descargan silenciosamente. Instálalos usando las secciones anteriores.

## Desarrollo

Además de las herramientas esenciales necesitas:

- Node.js 20 o posterior y npm.
- Rust estable mediante [rustup](https://rustup.rs/).
- Xcode Command Line Tools: `xcode-select --install`.

Ejecutar en desarrollo:

```sh
npm install
npm run dev
```

Crear la aplicación y el DMG:

```sh
npm run build
```

Ejecutar pruebas:

```sh
npm test -- --run
cargo test --offline --manifest-path src-tauri/Cargo.toml
npm run web:build
```

## Cómo se guardan los proyectos

Un proyecto contiene un archivo `.casicine` y una carpeta interna `.casi-cine` con SQLite, cachés, música generada y archivos temporales regenerables. Las rutas a los originales se guardan como referencias; las fotos y videos fuente permanecen intactos.

Al volver a generar con el mismo proyecto, Casi Cine reutiliza huellas, metadatos, análisis visual, transcripción y música que ya estén completos. Cambiar duración, estilo o formato de exportación no obliga a repetir todo el análisis.

## Arquitectura

```text
src/                         interfaz React y cliente Tauri
src-tauri/src/media.rs       descubrimiento, fechas, orientación y huellas
src-tauri/src/culling.rs     calidad, similitud y selección inicial
src-tauri/src/semantic.rs    Qwen, Whisper, eventos y caché narrativa
src-tauri/src/timeline.rs    capítulos y presupuesto de duración
src-tauri/src/location.rs    coordenadas EXIF y lugares locales
src-tauri/src/music.rs       música del usuario y adaptador ACE-Step
src-tauri/src/render.rs      encuadre, audio, títulos y exportación
src-tauri/src/db.rs          proyecto SQLite y recuperación
scripts/                     instalación y sidecar musical local
```

## Privacidad y red

Durante el uso normal, Casi Cine invoca únicamente ejecutables locales y lee los archivos seleccionados. Internet se utiliza solo cuando el usuario instala dependencias o modelos. El proyecto no incluye analítica, inicio de sesión, sincronización, actualizador remoto ni servicios de nube.

## Limitaciones conocidas

- El entendimiento visual es una inferencia y puede escoger un momento imperfecto; los controles de exclusión/restauración permanecen disponibles.
- Una fecha sin zona horaria puede ser incierta. Casi Cine combina metadatos, nombre de archivo y contexto cronológico, pero no modifica el original.
- Los títulos de ubicación solo aparecen cuando existe GPS fiable y la ubicación representa una etapa relevante.
- El intercambio editable es JSON; no se exporta todavía FCPXML ni CMX EDL.
- La primera generación profunda puede tardar bastante con cientos de archivos. El progreso se guarda y las siguientes regeneraciones reutilizan la caché.
