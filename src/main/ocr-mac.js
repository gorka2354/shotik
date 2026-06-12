#!/usr/bin/osascript -l JavaScript
// Apple Vision OCR via JXA (experimental). Usage: osascript -l JavaScript ocr-mac.js <image.png>
// Recognizes text in the user's preferred languages; prints one line per text block.
ObjC.import('Foundation');
ObjC.import('Vision');
ObjC.import('AppKit');

function run(argv) {
  const path = argv[0];
  const url = $.NSURL.fileURLWithPath(path);
  const handler = $.VNImageRequestHandler.alloc.initWithURLOptions(url, $.NSDictionary.dictionary);
  const request = $.VNRecognizeTextRequest.alloc.init;
  request.recognitionLevel = $.VNRequestTextRecognitionLevelAccurate;
  request.usesLanguageCorrection = true;

  const error = Ref();
  const ok = handler.performRequestsError($.NSArray.arrayWithObject(request), error);
  if (!ok) return 'OCR error';

  const results = request.results;
  const lines = [];
  for (let i = 0; i < results.count; i++) {
    const candidate = results.objectAtIndex(i).topCandidates(1);
    if (candidate.count > 0) lines.push(ObjC.unwrap(candidate.objectAtIndex(0).string));
  }
  return lines.join('\n');
}
