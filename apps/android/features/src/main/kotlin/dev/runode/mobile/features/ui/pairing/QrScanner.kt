package dev.runode.mobile.features.ui.pairing

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.OptIn
import androidx.camera.core.CameraSelector
import androidx.camera.core.ExperimentalGetImage
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.mlkit.vision.barcode.BarcodeScanner
import com.google.mlkit.vision.barcode.BarcodeScannerOptions
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.common.InputImage
import dev.runode.mobile.features.R
import com.google.android.gms.tasks.Tasks
import dev.runode.mobile.features.model.UiText
import java.util.concurrent.Executors

/**
 * 相机取景加二维码识别：后置摄像头的画面铺满，ML Kit 认出的二维码原文交给 [onCode]（在主线程上，同一个码连着认出时只交一次）。
 * 没有相机权限时先向用户要；要不到、没有摄像头、相机开不了时交给 [onUnavailable] 说原因，自己什么都不画。
 */
@Composable
internal fun QrScanner(onCode: (String) -> Unit, onUnavailable: (UiText) -> Unit, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val code by rememberUpdatedState(onCode)
    val unavailable by rememberUpdatedState(onUnavailable)
    var permitted by remember { mutableStateOf(hasPermission(context)) }
    val request = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        permitted = granted
        if (!granted) unavailable(UiText.res(R.string.scanner_no_permission))
    }
    LaunchedEffect(Unit) {
        when {
            !context.packageManager.hasSystemFeature(PackageManager.FEATURE_CAMERA_ANY) ->
                unavailable(UiText.res(R.string.scanner_no_camera))
            !permitted -> request.launch(Manifest.permission.CAMERA)
        }
    }
    if (!permitted) return

    val previewView = remember { PreviewView(context).apply { scaleType = PreviewView.ScaleType.FILL_CENTER } }
    AndroidView(factory = { previewView }, modifier = modifier)
    DisposableEffect(lifecycleOwner) {
        val executor = Executors.newSingleThreadExecutor()
        val scanner = BarcodeScanning.getClient(
            BarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).build(),
        )
        var provider: ProcessCameraProvider? = null
        var disposed = false
        val future = ProcessCameraProvider.getInstance(context)
        future.addListener(
            {
                if (disposed) return@addListener
                try {
                    val cameras = future.get()
                    provider = cameras
                    val preview = Preview.Builder().build().also { it.surfaceProvider = previewView.surfaceProvider }
                    val analysis = ImageAnalysis.Builder()
                        .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                        .build()
                    analysis.setAnalyzer(executor, QrAnalyzer(scanner, context) { code(it) })
                    cameras.unbindAll()
                    cameras.bindToLifecycle(lifecycleOwner, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis)
                } catch (e: Exception) {
                    unavailable(UiText.res(R.string.scanner_failed))
                }
            },
            ContextCompat.getMainExecutor(context),
        )
        onDispose {
            disposed = true
            provider?.unbindAll()
            scanner.close()
            executor.shutdown()
        }
    }
}

private fun hasPermission(context: Context): Boolean =
    ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED

/** 一帧一帧交给 ML Kit 认码，前一帧认完才收下一帧；认出的码切回主线程交出去，和上一次一样的不再交。 */
private class QrAnalyzer(
    private val scanner: BarcodeScanner,
    context: Context,
    private val onCode: (String) -> Unit,
) : ImageAnalysis.Analyzer {
    private val main = ContextCompat.getMainExecutor(context)
    private var last: String? = null

    @OptIn(ExperimentalGetImage::class)
    override fun analyze(image: ImageProxy) {
        val media = image.image
        if (media == null) {
            image.close()
            return
        }
        val found = runCatching {
            awaitBlocking(scanner, InputImage.fromMediaImage(media, image.imageInfo.rotationDegrees))
        }.getOrNull()
        image.close()
        if (found != null && found != last) {
            last = found
            main.execute { onCode(found) }
        }
    }

    /** 在分析线程上等 ML Kit 认完这一帧，交回第一个认出的码。 */
    private fun awaitBlocking(scanner: BarcodeScanner, input: InputImage): String? =
        Tasks.await(scanner.process(input)).firstNotNullOfOrNull { it.rawValue }
}
