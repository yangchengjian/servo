package org.mozilla.servo;

import android.app.Activity;
import android.content.Context;
import android.content.res.AssetManager;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.opengl.GLUtils;
import android.util.Log;
import java.io.IOException;

/** JNI interface to native layer. */
public class JniInterface {

  private static final String TAG = "JniInterface";
  static AssetManager assetManager;

  public static Bitmap loadImage(String imageName) {

    try {
      return BitmapFactory.decodeStream(assetManager.open(imageName));
    } catch (IOException e) {
      Log.e(TAG, "Cannot open image " + imageName);
      return null;
    }
  }

}
