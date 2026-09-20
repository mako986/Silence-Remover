use std::env;
use std::fs::{self};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

/// Доступные форматы выходного аудио
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioFormat {
    Mp3,
    Wav,
}

/// Конфигурация обработки аудиофайла
#[derive(Debug, Clone)]
struct AppConfig {
    input_path: PathBuf,
    output_path: PathBuf,
    noise_threshold_db: f32, // Порог шума/тишины в дБ (например, -35.0)
    min_silence_sec: f32,    // Минимальная длительность тишины для удаления (сек)
    output_format: AudioFormat,
    ai_transcribe: bool,     // Включить расшифровку через ИИ
    groq_api_key: Option<String>,
    gemini_api_key: Option<String>,
    deepseek_api_key: Option<String>,
}

/// Пути к исполняемым файлам FFmpeg и ffprobe
#[derive(Debug, Clone)]
struct FfmpegPaths {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

impl AudioFormat {
    fn from_ext(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "mp3" => Some(AudioFormat::Mp3),
            "wav" => Some(AudioFormat::Wav),
            _ => None,
        }
    }

    fn as_ext(&self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "mp3",
            AudioFormat::Wav => "wav",
        }
    }

    fn default_codec(&self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "libmp3lame",
            AudioFormat::Wav => "pcm_s16le",
        }
    }
}

/// Автоматически скачивает FFmpeg в локальную папку `ffmpeg_bin`, если он не установлен
fn download_ffmpeg(bin_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(bin_dir).map_err(|e| format!("Не удалось создать директорию: {}", e))?;

    println!("⏳ FFmpeg не обнаружен в системе. Начинаем автоматическую загрузку...");

    if cfg!(target_os = "windows") {
        let ps_script = format!(
            "[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; \
             $zip = '$env:TEMP\\ffmpeg_download.zip'; \
             $extract = '$env:TEMP\\ffmpeg_extracted'; \
             $url = 'https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip'; \
             Write-Host 'Скачивание FFmpeg для Windows...'; \
             Invoke-WebRequest -Uri $url -OutFile $zip; \
             Write-Host 'Распаковка архива...'; \
             Expand-Archive -Path $zip -DestinationPath $extract -Force; \
             Get-ChildItem -Path $extract -Filter 'ffmpeg.exe' -Recurse | Copy-Item -Destination '{0}\\ffmpeg.exe'; \
             Get-ChildItem -Path $extract -Filter 'ffprobe.exe' -Recurse | Copy-Item -Destination '{0}\\ffprobe.exe'; \
             Remove-Item -Path $zip -Force -ErrorAction SilentlyContinue; \
             Remove-Item -Path $extract -Recurse -Force -ErrorAction SilentlyContinue;",
            bin_dir.display()
        );

        let status = Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps_script])
            .status()
            .map_err(|e| format!("Ошибка выполнения PowerShell: {}", e))?;

        if !status.success() {
            return Err("Не удалось автоматически загрузить FFmpeg через PowerShell.".to_string());
        }
    } else if cfg!(target_os = "linux") {
        let sh_script = format!(
            "URL='https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz'; \
             TMP_XZ='/tmp/ffmpeg.tar.xz'; \
             TMP_EXT='/tmp/ffmpeg_extracted'; \
             curl -sSL \"$URL\" -o \"$TMP_XZ\" && \
             mkdir -p \"$TMP_EXT\" && \
             tar -xf \"$TMP_XZ\" -C \"$TMP_EXT\" && \
             find \"$TMP_EXT\" -name 'ffmpeg' -exec cp {{}} '{0}/ffmpeg' \\; && \
             find \"$TMP_EXT\" -name 'ffprobe' -exec cp {{}} '{0}/ffprobe' \\; && \
             chmod +x '{0}/ffmpeg' '{0}/ffprobe' && \
             rm -rf \"$TMP_XZ\" \"$TMP_EXT\"",
            bin_dir.display()
        );

        let status = Command::new("sh")
            .args(["-c", &sh_script])
            .status()
            .map_err(|e| format!("Ошибка выполнения shell-скрипта: {}", e))?;

        if !status.success() {
            return Err("Не удалось автоматически загрузить FFmpeg на Linux.".to_string());
        }
    } else if cfg!(target_os = "macos") {
        let sh_script = format!(
            "curl -sSL 'https://evermeet.cx/ffmpeg/getrelease/zip' -o /tmp/ffmpeg.zip && \
             curl -sSL 'https://evermeet.cx/ffmpeg/getrelease/ffprobe/zip' -o /tmp/ffprobe.zip && \
             unzip -o /tmp/ffmpeg.zip -d '{0}' && \
             unzip -o /tmp/ffprobe.zip -d '{0}' && \
             chmod +x '{0}/ffmpeg' '{0}/ffprobe' && \
             rm -f /tmp/ffmpeg.zip /tmp/ffprobe.zip",
            bin_dir.display()
        );

        let status = Command::new("sh")
            .args(["-c", &sh_script])
            .status()
            .map_err(|e| format!("Ошибка выполнения shell-скрипта: {}", e))?;

        if !status.success() {
            return Err("Не удалось автоматически загрузить FFmpeg на macOS.".to_string());
        }
    } else {
        return Err("Автоматическое скачивание не поддерживается для текущей ОС.".to_string());
    }

    println!("✅ FFmpeg успешно сохранен в локальную папку 'ffmpeg_bin'!");
    Ok(())
}

/// Проверяет доступность FFmpeg локально или в системном PATH.
fn ensure_ffmpeg_available() -> Result<FfmpegPaths, String> {
    let exe_ext = if cfg!(target_os = "windows") { ".exe" } else { "" };
    
    // 1. Локальная директория
    let local_bin_dir = PathBuf::from("ffmpeg_bin");
    let local_ffmpeg = local_bin_dir.join(format!("ffmpeg{}", exe_ext));
    let local_ffprobe = local_bin_dir.join(format!("ffprobe{}", exe_ext));

    if local_ffmpeg.exists() && local_ffprobe.exists() {
        return Ok(FfmpegPaths {
            ffmpeg: local_ffmpeg,
            ffprobe: local_ffprobe,
        });
    }

    // 2. Системный PATH
    let system_ffmpeg_status = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    if let Ok(status) = system_ffmpeg_status {
        if status.success() {
            return Ok(FfmpegPaths {
                ffmpeg: PathBuf::from("ffmpeg"),
                ffprobe: PathBuf::from("ffprobe"),
            });
        }
    }

    // 3. Скачиваем бинарники
    download_ffmpeg(&local_bin_dir)?;

    if local_ffmpeg.exists() && local_ffprobe.exists() {
        Ok(FfmpegPaths {
            ffmpeg: local_ffmpeg,
            ffprobe: local_ffprobe,
        })
    } else {
        Err("Не удалось загрузить бинарные файлы FFmpeg.".to_string())
    }
}

/// Измеряет длительность файла с помощью ffprobe
fn get_file_duration(path: &Path, paths: &FfmpegPaths) -> Result<f32, String> {
    let output = Command::new(&paths.ffprobe)
        .args([
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "default=noprint_wrappers=1:nokey=1",
            path.to_str().ok_or("Недопустимый путь к файлу")?,
        ])
        .output()
        .map_err(|e| format!("Ошибка запуска ffprobe: {}", e))?;

    if !output.status.success() {
        return Err("Не удалось прочитать длительность исходного файла.".to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.trim().parse::<f32>().map_err(|_| "Ошибка парсинга длительности".to_string())
}

/// Конструирует фильтр `silenceremove` с мгновенной реакцией на речь и мягким отступом
fn build_silence_filter(config: &AppConfig) -> String {
    format!(
        "silenceremove=start_periods=1:start_duration=0.02:start_threshold={:.1}dB:start_silence=0.15:stop_periods=-1:stop_duration={:.2}:stop_threshold={:.1}dB:stop_silence=0.15",
        config.noise_threshold_db,
        config.min_silence_sec,
        config.noise_threshold_db
    )
}

/// Выполняет обработку файла
fn process_audio_file(config: &AppConfig, paths: &FfmpegPaths) -> Result<(), String> {
    println!("▶ Обработка файла: {}", config.input_path.display());
    
    let filter_str = build_silence_filter(config);

    let mut cmd = Command::new(&paths.ffmpeg);
    cmd.arg("-y") // Перезапись без запроса
       .arg("-i")
       .arg(&config.input_path)
       .arg("-af")
       .arg(&filter_str)
       .arg("-c:a")
       .arg(config.output_format.default_codec());

    if config.output_format == AudioFormat::Mp3 {
        cmd.arg("-b:a").arg("192k");
    }

    cmd.arg(&config.output_path);

    let output = cmd.output().map_err(|e| format!("Ошибка запуска FFmpeg: {}", e))?;

    if !output.status.success() {
        let err_msg = String::from_utf8_lossy(&output.stderr);
        return Err(format!("FFmpeg завершился с ошибкой:\n{}", err_msg));
    }

    Ok(())
}

/// Отправляет аудиофайл в Groq Whisper API (Whisper-Large-V3) для супербыстрого распознавания речи
fn transcribe_with_groq(audio_path: &Path, api_key: &str) -> Result<String, String> {
    println!("⚡ Выполняем сверхбыстрое распознавание речи через Groq Whisper API...");

    let file_arg = format!("file=@{}", audio_path.display());

    let output = Command::new("curl")
        .args([
            "-s",
            "-X", "POST",
            "https://api.groq.com/openai/v1/audio/transcriptions",
            "-H", &format!("Authorization: Bearer {}", api_key),
            "-F", &file_arg,
            "-F", "model=whisper-large-v3",
            "-F", "response_format=json",
        ])
        .output()
        .map_err(|e| format!("Ошибка вызова curl: {}. Убедитесь, что curl установлен.", e))?;

    if !output.status.success() {
        return Err("Ошибка при отправке HTTP-запроса в Groq API через curl.".to_string());
    }

    let response_text = String::from_utf8_lossy(&output.stdout).to_string();

    if let Some(pos) = response_text.find("\"text\"") {
        if let Some(colon) = response_text[pos..].find(':') {
            let rest = &response_text[pos + colon + 1..];
            if let Some(first_quote) = rest.find('"') {
                let text_start = &rest[first_quote + 1..];
                if let Some(last_quote) = text_start.find('"') {
                    let raw_text = &text_start[..last_quote];
                    let clean_text = raw_text
                        .replace("\\n", "\n")
                        .replace("\\\"", "\"")
                        .replace("\\\\", "\\");
                    return Ok(clean_text);
                }
            }
        }
    }

    Err(format!("Не удалось распарсить ответ от Groq API:\n{}", response_text))
}

/// Улучшает и форматирует полученный текст через DeepSeek API
fn polish_text_with_deepseek(raw_text: &str, api_key: &str) -> Result<String, String> {
    println!("🧠 Редактируем и расставляем знаки препинания через DeepSeek API...");

    let temp_json_path = env::temp_dir().join("deepseek_req.json");
    let safe_text = raw_text.replace('"', "\\\"").replace('\n', "\\n");

    let json_body = format!(
        r#"{{
  "model": "deepseek-chat",
  "messages": [
    {{"role": "system", "content": "Ты - профессиональный редактор. Твоя задача: взять расшифрованный текст речи, исправить грамматические ошибки, расставить пунктуацию и грамотно разбить текст на смысловые абзацы. Не меняй смысл и слова спикера."}},
    {{"role": "user", "content": "{}"}}
  ]
}}"#,
        safe_text
    );

    fs::write(&temp_json_path, json_body).map_err(|e| format!("Ошибка записи запроса: {}", e))?;

    let output = Command::new("curl")
        .args([
            "-s",
            "-X", "POST",
            "-H", "Content-Type: application/json",
            "-H", &format!("Authorization: Bearer {}", api_key),
            "-d", &format!("@{}", temp_json_path.display()),
            "https://api.deepseek.com/v1/chat/completions",
        ])
        .output()
        .map_err(|e| format!("Ошибка вызова curl для DeepSeek: {}", e))?;

    let _ = fs::remove_file(temp_json_path);

    let response_text = String::from_utf8_lossy(&output.stdout).to_string();

    if let Some(pos) = response_text.find("\"content\":\"") {
        let start = pos + 11;
        if let Some(end) = response_text[start..].find("\"") {
            let res = &response_text[start..start + end];
            return Ok(res.replace("\\n", "\n").replace("\\\"", "\""));
        }
    }

    Err(format!("Ошибка ответа DeepSeek API:\n{}", response_text))
}

fn print_usage(exe_name: &str) {
    println!("\n📌 РУКОВОДСТВО ПО ИСПОЛЬЗОВАНИЮ:");
    println!("--------------------------------------------------");
    println!("  1. Командная строка:");
    println!("     {} <входной_файл> [выходной_файл] [опции]", exe_name);
    println!("     cargo run -- <входной_файл> [выходной_файл] [опции]\n");
    println!("  2. Опции:");
    println!("     -d, --db <число>         Порог тишины в дБ (по умолчанию: -35)");
    println!("     -p, --preset <название>  Пресеты:");
    println!("                              • pure   (-50 dB) - только 100% цифровая пустота");
    println!("                              • normal (-35 dB) - комната/микрофон (стандарт)");
    println!("                              • noisy  (-25 dB) - шумное помещение");
    println!("     -m, --min-sec <число>    Мин. длительность тишины в сек (по умолчанию: 0.4)");
    println!("     -f, --format <mp3|wav>   Формат файла (mp3 или wav)");
    println!("     -t, --transcribe         Включить ИИ-распознавание речи в .txt");
    println!("     --groq-key <ключ>        API ключ Groq Whisper (перем. среды GROQ_API_KEY) [Рекомендуется]");
    println!("     --gemini-key <ключ>      API ключ Gemini (перем. среды GEMINI_API_KEY)");
    println!("     --deepseek-key <ключ>    API ключ DeepSeek для постобработки текста");
    println!("     -h, --help               Показать это руководство\n");
}

/// Удаляет кавычки при перетаскивании файла в консоль
fn sanitize_path(path_str: &str) -> PathBuf {
    let trimmed = path_str.trim().trim_matches(|c| c == '"' || c == '\'');
    PathBuf::from(trimmed)
}

/// Ожидание нажатия Enter перед закрытием консоли
fn wait_for_key() {
    println!("\nНажмите Enter, чтобы закрыть окно...");
    let mut input = String::new();
    let _ = io::stdin().read_line(&mut input);
}

fn parse_args() -> Result<Option<(AppConfig, bool)>, String> {
    let raw_args: Vec<String> = env::args().collect();
    let exe_name = raw_args.first().cloned().unwrap_or_else(|| "silence_remover".to_string());
    
    if raw_args.contains(&"--help".to_string()) || raw_args.contains(&"-h".to_string()) {
        print_usage(&exe_name);
        return Ok(None);
    }

    let filtered_args: Vec<String> = raw_args
        .into_iter()
        .enumerate()
        .filter(|(idx, arg)| !(*idx == 1 && arg == "--") && *idx != 0)
        .map(|(_, arg)| arg)
        .collect();

    // Интерактивный режим (при клике без аргументов)
    if filtered_args.is_empty() {
        print_usage(&exe_name);
        println!("==================================================");
        println!("💡 ИНТЕРАКТИВНЫЙ РЕЖИМ (Прямой запуск)");
        println!("==================================================");
        print!("👉 Перетащите файл в это окно и нажмите Enter:\n> ");
        io::stdout().flush().ok();

        let mut input_line = String::new();
        io::stdin().read_line(&mut input_line).map_err(|e| e.to_string())?;

        let trimmed = input_line.trim();
        if trimmed.is_empty() {
            println!("Файл не передан. Завершение работы.");
            return Ok(None);
        }

        let input_path = sanitize_path(trimmed);
        if !input_path.exists() {
            return Err(format!("Файл не найден: {}", input_path.display()));
        }

        println!("\nВыберите пресет чувствительности:");
        println!("  [1] normal (-35 dB) - Стандарт для речи (по умолчанию)");
        println!("  [2] pure   (-50 dB) - Только абсолютная тишина");
        println!("  [3] noisy  (-25 dB) - Агрессивная обрезка (для шума)");
        print!("Ваш выбор [1-3]: ");
        io::stdout().flush().ok();

        let mut preset_line = String::new();
        io::stdin().read_line(&mut preset_line).ok();

        let noise_threshold_db = match preset_line.trim() {
            "2" | "pure" => -50.0,
            "3" | "noisy" => -25.0,
            _ => -35.0,
        };

        // Спрашиваем про расшифровку в текст
        print!("\n📝 Выполнить ИИ-расшифровку речи в .txt файл? (y/n / д/н): ");
        io::stdout().flush().ok();

        let mut stt_line = String::new();
        io::stdin().read_line(&mut stt_line).ok();
        
        let stt_ans = stt_line.trim().to_lowercase();
        let ai_transcribe = matches!(stt_ans.as_str(), "y" | "yes" | "д" | "да" | "1" | "у");

        let mut groq_key = env::var("GROQ_API_KEY").ok();
        let gemini_key = env::var("GEMINI_API_KEY").ok();
        let mut deepseek_key = env::var("DEEPSEEK_API_KEY").ok();

        if ai_transcribe {
            println!("\n--------------------------------------------------");
            println!("🔑 НАСТРОЙКА ИИ-КЛЮЧЕЙ");
            println!("--------------------------------------------------");
            
            if let Some(ref k) = groq_key {
                println!("✅ Найден Groq API Key в системе: {}...", &k[..k.len().min(8)]);
            } else {
                print!("👉 Вставьте ваш Groq API Key (gsk_...) и нажмите Enter:\n> ");
                io::stdout().flush().ok();
                let mut key_line = String::new();
                io::stdin().read_line(&mut key_line).ok();
                let key_trimmed = key_line.trim().to_string();
                if !key_trimmed.is_empty() {
                    groq_key = Some(key_trimmed);
                }
            }

            if deepseek_key.is_none() {
                print!("\n🧠 (Опционально) Введите DeepSeek API Key для пунктуации [Enter - пропустить]:\n> ");
                io::stdout().flush().ok();
                let mut ds_line = String::new();
                io::stdin().read_line(&mut ds_line).ok();
                let ds_trimmed = ds_line.trim().to_string();
                if !ds_trimmed.is_empty() {
                    deepseek_key = Some(ds_trimmed);
                }
            }
        }

        let stem = input_path.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
        let output_path = input_path.with_file_name(format!("{}_clean.mp3", stem));

        let config = AppConfig {
            input_path,
            output_path,
            noise_threshold_db,
            min_silence_sec: 0.4,
            output_format: AudioFormat::Mp3,
            ai_transcribe,
            groq_api_key: groq_key,
            gemini_api_key: gemini_key,
            deepseek_api_key: deepseek_key,
        };

        return Ok(Some((config, true)));
    }

    // Режим командной строки
    let mut positional_args: Vec<PathBuf> = Vec::new();
    let mut noise_threshold_db: f32 = -35.0;
    let mut min_silence_sec: f32 = 0.4;
    let mut custom_format: Option<AudioFormat> = None;
    let mut ai_transcribe = false;
    let mut groq_api_key = env::var("GROQ_API_KEY").ok();
    let mut gemini_api_key = env::var("GEMINI_API_KEY").ok();
    let mut deepseek_api_key = env::var("DEEPSEEK_API_KEY").ok();

    let mut idx = 0;
    while idx < filtered_args.len() {
        let arg = &filtered_args[idx];

        match arg.as_str() {
            "--db" | "-d" => {
                idx += 1;
                if idx < filtered_args.len() {
                    noise_threshold_db = filtered_args[idx].parse::<f32>()
                        .map_err(|_| "Некорректное значение дБ для --db")?;
                }
            }
            "--preset" | "-p" => {
                idx += 1;
                if idx < filtered_args.len() {
                    match filtered_args[idx].to_lowercase().as_str() {
                        "pure" | "clean" => noise_threshold_db = -50.0,
                        "normal" | "medium" => noise_threshold_db = -35.0,
                        "noisy" | "aggressive" => noise_threshold_db = -25.0,
                        _ => return Err("Неизвестный пресет. Используйте: pure, normal или noisy".to_string()),
                    }
                }
            }
            "--min-sec" | "-m" => {
                idx += 1;
                if idx < filtered_args.len() {
                    min_silence_sec = filtered_args[idx].parse::<f32>()
                        .map_err(|_| "Некорректное значение секунд для --min-sec")?;
                }
            }
            "--format" | "-f" => {
                idx += 1;
                if idx < filtered_args.len() {
                    custom_format = AudioFormat::from_ext(&filtered_args[idx]);
                }
            }
            "--transcribe" | "-t" => {
                ai_transcribe = true;
            }
            "--groq-key" => {
                idx += 1;
                if idx < filtered_args.len() {
                    groq_api_key = Some(filtered_args[idx].clone());
                }
            }
            "--gemini-key" => {
                idx += 1;
                if idx < filtered_args.len() {
                    gemini_api_key = Some(filtered_args[idx].clone());
                }
            }
            "--deepseek-key" => {
                idx += 1;
                if idx < filtered_args.len() {
                    deepseek_api_key = Some(filtered_args[idx].clone());
                }
            }
            _ if arg.starts_with('-') => {
                return Err(format!("Неизвестная опция: {}", arg));
            }
            _ => {
                positional_args.push(PathBuf::from(arg));
            }
        }
        idx += 1;
    }

    if positional_args.is_empty() {
        return Err("Не указан входной файл!".to_string());
    }

    let input_path = positional_args[0].clone();
    if !input_path.exists() {
        return Err(format!("Входной файл не найден: {}", input_path.display()));
    }

    let output_path_opt = positional_args.get(1).cloned();

    let output_format = if let Some(fmt) = custom_format {
        fmt
    } else if let Some(ref path) = output_path_opt {
        path.extension()
            .and_then(|ext| ext.to_str())
            .and_then(AudioFormat::from_ext)
            .unwrap_or(AudioFormat::Mp3)
    } else {
        AudioFormat::Mp3
    };

    let final_output_path = output_path_opt.unwrap_or_else(|| {
        let stem = input_path.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
        input_path.with_file_name(format!("{}_clean.{}", stem, output_format.as_ext()))
    });

    Ok(Some((
        AppConfig {
            input_path,
            output_path: final_output_path,
            noise_threshold_db,
            min_silence_sec,
            output_format,
            ai_transcribe,
            groq_api_key,
            gemini_api_key,
            deepseek_api_key,
        },
        false,
    )))
}

fn format_duration(seconds: f32) -> String {
    let mins = (seconds / 60.0).floor() as u32;
    let secs = seconds % 60.0;
    if mins > 0 {
        format!("{} м {:.1} с", mins, secs)
    } else {
        format!("{:.2} с", secs)
    }
}

fn print_header() {
    println!("==================================================");
    println!("    Silence Remover & AI Transcriber (Rust)");
    println!("==================================================");
}

fn main() {
    print_header();

    // 1. Проверяем или скачиваем FFmpeg
    let ffmpeg_paths = match ensure_ffmpeg_available() {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("\n❌ Ошибка подготовки FFmpeg: {}", e);
            wait_for_key();
            std::process::exit(1);
        }
    };

    // 2. Парсим аргументы
    let (config, is_interactive) = match parse_args() {
        Ok(Some(res)) => res,
        Ok(None) => return,
        Err(e) => {
            eprintln!("\n❌ Ошибка параметров: {}", e);
            wait_for_key();
            std::process::exit(1);
        }
    };

    println!("\n Настройки:");
    println!(" ├ Вход:         {}", config.input_path.display());
    println!(" ├ Выход:        {}", config.output_path.display());
    println!(" ├ Формат:       {}", config.output_format.as_ext().to_uppercase());
    println!(" ├ Порог:        {:.1} dB", config.noise_threshold_db);
    println!(" ├ Мин. пауза:   {:.2} сек", config.min_silence_sec);
    println!(" └ ИИ Расшифровка: {}", if config.ai_transcribe { "ВКЛЮЧЕНА" } else { "выключена" });
    println!("--------------------------------------------------");

    // 3. Измеряем длительность
    let original_duration = get_file_duration(&config.input_path, &ffmpeg_paths).unwrap_or(0.0);
    if original_duration > 0.0 {
        println!("⏱ Исходная длительность: {}", format_duration(original_duration));
    }

    // 4. Обработка
    let start_time = Instant::now();
    match process_audio_file(&config, &ffmpeg_paths) {
        Ok(_) => {
            let elapsed = start_time.elapsed();
            println!("\n✅ Аудио очищено за {:.2} сек!", elapsed.as_secs_f32());

            // 5. Вычисляем итоговые метрики
            let new_duration = get_file_duration(&config.output_path, &ffmpeg_paths).unwrap_or(0.0);
            if original_duration > 0.0 && new_duration > 0.0 {
                let saved = original_duration - new_duration;
                let percent = (saved / original_duration) * 100.0;
                println!("--------------------------------------------------");
                println!("📊 Итоги очистки:");
                println!(" ├ Итоговая длительность: {}", format_duration(new_duration));
                println!(" └ Вырезано тишины:        {} ({:.1}%)", format_duration(saved), percent);
            }

            // 6. ИИ Расшифровка текста (если включена)
            if config.ai_transcribe {
                println!("\n--------------------------------------------------");
                let transcribe_res = if let Some(ref groq_key) = config.groq_api_key {
                    transcribe_with_groq(&config.output_path, groq_key)
                } else {
                    Err("Не указан API ключ (Groq)".to_string())
                };

                match transcribe_res {
                    Ok(mut transcript) => {
                        println!("✅ Распознавание завершено!");

                        // Если передали ключ DeepSeek, дополнительно отдаем ему на обработку
                        if let Some(ref ds_key) = config.deepseek_api_key {
                            match polish_text_with_deepseek(&transcript, ds_key) {
                                Ok(polished) => transcript = polished,
                                Err(e) => eprintln!("⚠️ DeepSeek редактор пропущен: {}", e),
                            }
                        }

                        // Сохраняем рядом в .txt файл
                        let txt_path = config.output_path.with_extension("txt");
                        if let Ok(_) = fs::write(&txt_path, &transcript) {
                            println!("📄 Текст сохранен в: {}", txt_path.display());
                        }
                    }
                    Err(e) => {
                        eprintln!("❌ Ошибка ИИ расшифровки: {}", e);
                        eprintln!("💡 Подсказка: убедитесь, что указали ключ через --groq-key или переменную окружения GROQ_API_KEY");
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("\n❌ Ошибка обработки:\n{}", e);
            if is_interactive {
                wait_for_key();
            }
            std::process::exit(1);
        }
    }

    if is_interactive {
        wait_for_key();
    }
}