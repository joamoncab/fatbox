import express from "express";
import multer from "multer";
import fs from "fs";
import path from "path";
import FormData from "form-data";
import axios from "axios";
import https from "https";

const app = express();
const PORT = process.env.PORT || 3000;

app.use(express.urlencoded({ extended: true }));
app.use(express.json());

const uploadsDir = path.join("/tmp", "uploads");
const tempDir = path.join("/tmp", "temp");

fs.mkdirSync(uploadsDir, { recursive: true });
fs.mkdirSync(tempDir, { recursive: true });

const chunkStorage = multer.diskStorage({
  destination: (req, file, cb) => {
    const { uploadId } = req.body;
    if (!uploadId) return cb(new Error("Missing uploadId"));
    const uploadPath = path.join(uploadsDir, uploadId);
    fs.mkdirSync(uploadPath, { recursive: true });
    cb(null, uploadPath);
  },
  filename: (req, file, cb) => {
    const { index } = req.body;
    if (typeof index === "undefined") return cb(new Error("Missing index"));
    cb(null, `chunk_${index}`);
  }
});

const chunkUpload = multer({ storage: chunkStorage });
const directUpload = multer({ dest: tempDir });

app.post("/chunk", chunkUpload.single("chunk"), (req, res) => {
  const { uploadId, index } = req.body;
  if (!uploadId || typeof index === "undefined") {
    console.warn("❌ Chunk upload missing uploadId or index");
    return res.status(400).json({ error: "Missing uploadId or index" });
  }

  console.log(`✅ Received chunk ${index} for uploadId: ${uploadId}`);
  res.json({ message: `Chunk ${index} for ${uploadId} received.` });
});

app.post("/finish", async (req, res) => {
  try {
    const { uploadId, filename, userhash, destination, time = "1h" } = req.body;

    if (!uploadId || !filename || !destination) {
      console.warn("❌ Finish called with missing parameters");
      return res.status(400).json({ error: "Missing uploadId, filename, or destination" });
    }

    const chunkDir = path.join(uploadsDir, uploadId);
    const finalFilePath = path.join(tempDir, `${uploadId}-${filename}`);

    if (!fs.existsSync(chunkDir)) {
      console.warn(`❌ No chunks found for uploadId ${uploadId}`);
      return res.status(400).json({ error: "No chunks found for this uploadId" });
    }

    console.log(`🔧 Reassembling chunks for uploadId ${uploadId}...`);
    const chunkFiles = fs
      .readdirSync(chunkDir)
      .filter((f) => f.startsWith("chunk_"))
      .sort((a, b) => parseInt(a.split("_")[1]) - parseInt(b.split("_")[1]));

    const writeStream = fs.createWriteStream(finalFilePath);
    for (const chunk of chunkFiles) {
      const chunkPath = path.join(chunkDir, chunk);
      const data = fs.readFileSync(chunkPath);
      writeStream.write(data);
    }
    writeStream.end();

    await new Promise((resolve) => writeStream.on("finish", resolve));
    console.log(`📦 Assembled file ready: ${finalFilePath}`);

    const url = await forwardToDestination(destination, finalFilePath, filename, userhash, time);
    console.log(`🚀 Uploaded to ${destination}: ${url}`);

    fs.rmSync(chunkDir, { recursive: true, force: true });
    fs.unlinkSync(finalFilePath);

    res.json({ url });
  } catch (err) {
    console.error("❌ Upload error:", err);
    res.status(500).json({ error: "Upload failed", details: err.message });
  }
});

app.post("/direct", directUpload.single("file"), async (req, res) => {
  try {
    const { destination, time = "1h", userhash } = req.body;
    if (!req.file || !destination) {
      console.warn("❌ Direct upload missing file or destination");
      return res.status(400).json({ error: "Missing file or destination" });
    }

    const filePath = req.file.path;
    const originalName = req.file.originalname || "upload.dat";
    console.log(`📥 Direct upload received: ${originalName} -> ${destination}`);

    const url = await forwardToDestination(destination, filePath, originalName, userhash, time);
    console.log(`🚀 Direct upload complete: ${url}`);

    fs.unlinkSync(filePath);
    res.json({ url });
  } catch (err) {
    console.error("❌ Direct upload error:", err);
    res.status(500).json({ error: "Direct upload failed", details: err.message });
  }
});

async function forwardToDestination(destination, filePath, originalName, userhash, time) {
  if (destination === "pomf") {
    return new Promise((resolve, reject) => {
      const form = new FormData();
      form.append("files[]", fs.createReadStream(filePath), originalName);

      const req = https.request({
        method: "POST",
        host: "pomf.lain.la",
        path: "/upload.php",
        headers: form.getHeaders(),
      }, (res) => {
        let data = "";
        res.on("data", chunk => data += chunk);
        res.on("end", () => {
          try {
            const json = JSON.parse(data);
            if (json.success && json.files?.[0]?.url) {
              resolve(json.files[0].url);
            } else {
              reject(new Error("Pomf upload failed: " + data));
            }
          } catch (err) {
            reject(new Error("Pomf JSON parse error: " + err.message));
          }
        });
      });

      req.on("error", err => reject(err));
      form.pipe(req);
    });
  }

  const form = new FormData();
  form.append("reqtype", "fileupload");
  form.append("fileToUpload", fs.createReadStream(filePath), originalName);
  if (destination === "catbox" && userhash) form.append("userhash", userhash);
  if (destination === "litterbox") form.append("time", time);

  const url = destination === "catbox"
    ? "https://catbox.moe/user/api.php"
    : "https://litterbox.catbox.moe/resources/internals/api.php";

  const response = await axios.post(url, form, {
    headers: form.getHeaders(),
    maxBodyLength: Infinity,
    maxContentLength: Infinity,
  });

  return response.data;
}

app.get("/", (req, res) => {
  res.status(200).send("fatbox is working.");
});

app.use((req, res) => {
  res.status(404).json({
    message: `Route ${req.method}:${req.path} not found`,
    error: "Not Found",
    statusCode: 404,
  });
});

app.listen(PORT, () => {
  console.log(`✅ Server listening on http://localhost:${PORT}`);
});