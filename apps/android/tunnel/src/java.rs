//! Kotlin 的 `dev.runode.mobile.connection.IrohLibrary` 上的 native 方法，薄薄地包住 crate 根上的几个函数：
//! 编号原样进出，出错时抛 `java.io.IOException`（`node` 不是公钥时抛 `IllegalArgumentException`），panic
//! 抛 `RuntimeException`。

use std::{io, time::Duration};

use jni::{
    Env, EnvUnowned,
    errors::ThrowRuntimeExAndDefault,
    jni_str,
    objects::{JByteArray, JClass, JObject, JString},
    strings::JNIString,
    sys::{jint, jlong},
};

/// 抛出 `err` 对应的 Java 异常。返回的总是 `Err`（异常已经挂上），交给 `with_env` 的调用方照规矩收尾。
fn throw<T>(env: &mut Env<'_>, err: &io::Error) -> jni::errors::Result<T> {
    let class = match err.kind() {
        io::ErrorKind::InvalidInput => jni_str!("java/lang/IllegalArgumentException"),
        _ => jni_str!("java/io/IOException"),
    };
    env.throw_new(class, JNIString::from(err.to_string()))?;
    Err(jni::errors::Error::JavaException)
}

fn throwing<T>(env: &mut Env<'_>, result: io::Result<T>) -> jni::errors::Result<T> {
    result.or_else(|err| throw(env, &err))
}

/// 校验 `array[offset, offset + length)` 在界内，返回两个数的 `usize`；越界时抛 `IndexOutOfBoundsException`。
fn region(
    env: &mut Env<'_>,
    array: &JByteArray<'_>,
    offset: jint,
    length: jint,
) -> jni::errors::Result<(usize, usize)> {
    let size = array.len(env)?;
    match (usize::try_from(offset), usize::try_from(length)) {
        (Ok(offset), Ok(length)) if offset.checked_add(length).is_some_and(|end| end <= size) => Ok((offset, length)),
        _ => {
            env.throw_new(
                jni_str!("java/lang/IndexOutOfBoundsException"),
                JNIString::from(format!("offset {offset}, length {length}, array size {size}")),
            )?;
            Err(jni::errors::Error::JavaException)
        }
    }
}

fn as_jbytes(bytes: &[u8]) -> &[i8] {
    // SAFETY: u8 和 i8 大小、对齐都一样，任何位模式都合法；长度和生命周期照搬 `bytes`。
    unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<i8>(), bytes.len()) }
}

fn as_bytes(bytes: &[i8]) -> &[u8] {
    // SAFETY: 同 `as_jbytes`，方向相反。
    unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<u8>(), bytes.len()) }
}

/// 把 Application context 交给 iroh，它经 JNI 读系统的 DNS 设置；没交时它用自带的公共 DNS。整个进程只交一次，
/// 交出的全局引用一直不放。
#[cfg(target_os = "android")]
fn install_context(env: &mut Env<'_>, context: &JObject<'_>) -> jni::errors::Result<()> {
    static INSTALLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if context.is_null() || INSTALLED.get().is_some() {
        return Ok(());
    }
    let vm = env.get_java_vm()?;
    let context = env.new_global_ref(context)?;
    INSTALLED.get_or_init(|| {
        // SAFETY: JavaVM 活到进程结束；context 是从不删除的全局引用，也活到进程结束。
        unsafe { iroh::dns::install_android_jni_context(vm.get_raw().cast(), context.into_raw().cast()) }
    });
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn install_context(_env: &mut Env<'_>, _context: &JObject<'_>) -> jni::errors::Result<()> {
    Ok(())
}

/// `bind(context: Any?): Long`：`context` 是 Application context，可以为空。
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_runode_mobile_connection_IrohLibrary_bind<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    context: JObject<'local>,
) -> jlong {
    env.with_env(|env| {
        install_context(env, &context)?;
        throwing(env, crate::bind())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `connect(endpoint: Long, node: String, alpn: ByteArray, timeoutMillis: Long): Long`。
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_runode_mobile_connection_IrohLibrary_connect<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    endpoint: jlong,
    node: JString<'local>,
    alpn: JByteArray<'local>,
    timeout_millis: jlong,
) -> jlong {
    env.with_env(|env| {
        let node = node.try_to_string(env)?;
        let alpn = env.convert_byte_array(&alpn)?;
        let timeout = Duration::from_millis(u64::try_from(timeout_millis).unwrap_or(0));
        throwing(env, crate::connect(endpoint, &node, &alpn, timeout))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `read(stream: Long, buffer: ByteArray, offset: Int, length: Int): Int`：读到的字节数，对面说完了时为 -1。
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_runode_mobile_connection_IrohLibrary_read<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    stream: jlong,
    buffer: JByteArray<'local>,
    offset: jint,
    length: jint,
) -> jint {
    env.with_env(|env| -> jni::errors::Result<jint> {
        // 每条读线程留一块缓冲反复用，不每读一次分配一块。
        thread_local! {
            static CHUNK: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        let (_, length) = region(env, &buffer, offset, length)?;
        CHUNK.with_borrow_mut(|chunk| {
            chunk.resize(length, 0);
            let Some(count) = throwing(env, crate::read(stream, &mut chunk[..length]))? else {
                return Ok(-1);
            };
            buffer.set_region(env, offset, as_jbytes(&chunk[..count]))?;
            // `count` 不超过 `length`，而 `length` 本来就是 jint。
            Ok(jint::try_from(count).unwrap_or(jint::MAX))
        })
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `write(stream: Long, buffer: ByteArray, offset: Int, length: Int)`：写完才返回。
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_runode_mobile_connection_IrohLibrary_write<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    stream: jlong,
    buffer: JByteArray<'local>,
    offset: jint,
    length: jint,
) {
    env.with_env(|env| {
        let (_, length) = region(env, &buffer, offset, length)?;
        let mut chunk = vec![0; length];
        buffer.get_region(env, offset, &mut chunk)?;
        throwing(env, crate::write(stream, as_bytes(&chunk)))
    })
    .resolve::<ThrowRuntimeExAndDefault>();
}

/// `close(stream: Long)`：可以从别的线程调，打断阻塞着的 `read`、`write`。
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_runode_mobile_connection_IrohLibrary_close<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    stream: jlong,
) {
    env.with_env(|_| -> jni::errors::Result<()> {
        crate::close(stream);
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>();
}

/// `closeEndpoint(endpoint: Long)`。
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_runode_mobile_connection_IrohLibrary_closeEndpoint<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    endpoint: jlong,
) {
    env.with_env(|_| -> jni::errors::Result<()> {
        crate::close_endpoint(endpoint);
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>();
}
