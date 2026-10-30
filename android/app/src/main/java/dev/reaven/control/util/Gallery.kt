package dev.reaven.control.util

import android.content.ContentValues
import android.content.Context
import android.graphics.Bitmap
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asAndroidBitmap
import java.io.File
import java.io.FileOutputStream
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/** Saves captured frames so they land in the phone's photo gallery. */
object Gallery {

    private fun filename(): String {
        val stamp = SimpleDateFormat("yyyyMMdd-HHmmss", Locale.US).format(Date())
        return "reaven-$stamp.png"
    }

    fun save(context: Context, image: ImageBitmap) = save(context, image.asAndroidBitmap())

    fun save(context: Context, bitmap: Bitmap) {
        val name = filename()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            val values = ContentValues().apply {
                put(MediaStore.Images.Media.DISPLAY_NAME, name)
                put(MediaStore.Images.Media.MIME_TYPE, "image/png")
                put(
                    MediaStore.Images.Media.RELATIVE_PATH,
                    Environment.DIRECTORY_PICTURES + "/Reaven"
                )
            }
            val resolver = context.contentResolver
            val uri = resolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values)
                ?: throw IllegalStateException("could not create gallery entry")
            resolver.openOutputStream(uri)?.use { stream ->
                bitmap.compress(Bitmap.CompressFormat.PNG, 100, stream)
            } ?: throw IllegalStateException("could not open output stream")
        } else {
            // Legacy path: app-specific Pictures directory, no permission needed.
            val dir = context.getExternalFilesDir(Environment.DIRECTORY_PICTURES)
                ?: File(context.filesDir, "pictures")
            if (!dir.exists() && !dir.mkdirs()) {
                throw IllegalStateException("could not create ${dir.path}")
            }
            FileOutputStream(File(dir, name)).use { stream ->
                bitmap.compress(Bitmap.CompressFormat.PNG, 100, stream)
            }
        }
    }
}
