// W13-K: the Photoshop DOM subset File > Script exposes, written over ONE
// native function, `__host(op, argsJson) -> replyJson`, which the Rust side
// (`script::host`) answers with editor commands. Evaluated before every
// script; the native is then removed from the global object, so a script
// reaches the editor only through the objects below.
(function (g) {
  "use strict";
  var host = g.__host;
  delete g.__host;

  function call(op) {
    var args = Array.prototype.slice.call(arguments, 1);
    var reply = JSON.parse(host(op, JSON.stringify(args)));
    if (reply.e !== undefined) {
      throw new Error(reply.e);
    }
    return reply.v;
  }

  function show(value) {
    if (typeof value === "string") {
      return value;
    }
    if (value === undefined) {
      return "undefined";
    }
    try {
      var json = JSON.stringify(value);
      return json === undefined ? String(value) : json;
    } catch (e) {
      return String(value);
    }
  }

  function joined(args) {
    return Array.prototype.map.call(args, show).join(" ");
  }

  // ---- output -------------------------------------------------------------
  g.alert = function (message) {
    call("log", "alert", show(message));
  };
  var console = {};
  ["log", "info", "warn", "error", "debug"].forEach(function (name) {
    console[name] = function () {
      call("log", name === "error" || name === "warn" ? "error" : "output", joined(arguments));
    };
  });
  g.console = console;
  g.print = console.log;
  g.$ = {
    writeln: function () { call("log", "output", joined(arguments)); },
    write: function () { call("log", "output", joined(arguments)); }
  };

  // ---- constants ----------------------------------------------------------
  var consts = call("consts");
  var BlendMode = {};
  consts.blend.forEach(function (name) { BlendMode[name.toUpperCase()] = name; });
  g.BlendMode = BlendMode;
  g.ColorBlendMode = BlendMode;
  g.LayerKind = {
    NORMAL: "normal", TEXT: "text", SMARTOBJECT: "smartobject", SOLIDFILL: "fill",
    GRADIENTFILL: "fill", PATTERNFILL: "fill", SHAPE: "shape", ADJUSTMENT: "adjustment",
    GENERATOR: "generator"
  };
  g.AnchorPosition = {
    TOPLEFT: "topleft", TOPCENTER: "topcenter", TOPRIGHT: "topright",
    MIDDLELEFT: "middleleft", MIDDLECENTER: "middlecenter", MIDDLERIGHT: "middleright",
    BOTTOMLEFT: "bottomleft", BOTTOMCENTER: "bottomcenter", BOTTOMRIGHT: "bottomright"
  };
  g.ResampleMethod = {
    NEARESTNEIGHBOR: "nearest", BILINEAR: "bilinear", BICUBIC: "bicubic",
    BICUBICSMOOTHER: "bicubic", BICUBICSHARPER: "lanczos", AUTOMATIC: "bicubic",
    NONE: "none"
  };
  g.SelectionType = {
    REPLACE: "replace", EXTEND: "extend", DIMINISH: "diminish", INTERSECT: "intersect"
  };
  g.Units = { PIXELS: "px", POINTS: "pt", PERCENT: "%", INCHES: "in", CM: "cm", MM: "mm" };
  g.DialogModes = { NO: "no", ALL: "all", ERROR: "error" };
  g.DocumentFill = { WHITE: "white", BACKGROUNDCOLOR: "background", TRANSPARENT: "transparent" };
  g.NewDocumentMode = { RGB: "rgb" };
  g.SaveOptions = { DONOTSAVECHANGES: "no", SAVECHANGES: "yes", PROMPTTOSAVECHANGES: "ask" };

  // ---- colours ------------------------------------------------------------
  function clampByte(v) {
    v = Math.round(Number(v));
    return v < 0 ? 0 : (v > 255 ? 255 : (v === v ? v : 0));
  }
  function RGBColor(r, gr, b) {
    this.red = r || 0;
    this.green = gr || 0;
    this.blue = b || 0;
  }
  Object.defineProperty(RGBColor.prototype, "hexValue", {
    get: function () {
      return [this.red, this.green, this.blue].map(function (v) {
        var h = clampByte(v).toString(16).toUpperCase();
        return h.length < 2 ? "0" + h : h;
      }).join("");
    },
    set: function (hex) {
      var h = String(hex).replace(/^#/, "");
      if (!/^[0-9a-fA-F]{6}$/.test(h)) {
        throw new Error("hexValue must be six hex digits, got " + hex);
      }
      this.red = parseInt(h.slice(0, 2), 16);
      this.green = parseInt(h.slice(2, 4), 16);
      this.blue = parseInt(h.slice(4, 6), 16);
    }
  });
  function SolidColor() {
    this.rgb = new RGBColor(0, 0, 0);
  }
  g.RGBColor = RGBColor;
  g.SolidColor = SolidColor;
  function solid(rgb) {
    var c = new SolidColor();
    c.rgb.red = rgb[0];
    c.rgb.green = rgb[1];
    c.rgb.blue = rgb[2];
    return c;
  }
  function rgbOf(color) {
    if (color && color.rgb) {
      return [clampByte(color.rgb.red), clampByte(color.rgb.green), clampByte(color.rgb.blue)];
    }
    if (Array.isArray(color) && color.length >= 3) {
      return [clampByte(color[0]), clampByte(color[1]), clampByte(color[2])];
    }
    if (typeof color === "string") {
      var c = new RGBColor(0, 0, 0);
      c.hexValue = color;
      return [c.red, c.green, c.blue];
    }
    throw new Error("expected a SolidColor, [r, g, b] or a hex string");
  }

  // ---- a file is only a name ----------------------------------------------
  // A script cannot read or write a path of its own choosing: app.open and
  // saveAs ask the user with the platform picker. A File's name is only
  // written to the output log; the picker does not open at it.
  function File(path) {
    this.fsName = String(path === undefined ? "" : path);
    var parts = this.fsName.split(/[\\/]/);
    this.name = parts[parts.length - 1];
    this.fullName = this.fsName;
  }
  g.File = File;

  // ---- collections --------------------------------------------------------
  function collection(items, extra) {
    var list = items.slice();
    list.getByName = function (name) {
      for (var i = 0; i < list.length; i++) {
        if (list[i].name === name) {
          return list[i];
        }
      }
      throw new Error("No item named " + name);
    };
    for (var key in extra) {
      if (Object.prototype.hasOwnProperty.call(extra, key)) {
        list[key] = extra[key];
      }
    }
    return list;
  }

  function wrap(doc, info) {
    return info.group ? new LayerSet(doc, info.id) : new ArtLayer(doc, info.id);
  }

  function children(doc, parent, which) {
    var infos = call("doc.children", doc, parent);
    var out = [];
    infos.forEach(function (info) {
      if (which === "all" || (which === "sets") === info.group) {
        out.push(wrap(doc, info));
      }
    });
    return out;
  }

  function layerCollections(target, doc, parent) {
    Object.defineProperty(target, "layers", {
      get: function () { return collection(children(doc, parent, "all"), {}); }
    });
    Object.defineProperty(target, "artLayers", {
      get: function () {
        return collection(children(doc, parent, "art"), {
          add: function () { return wrap(doc, call("doc.addLayer", doc, parent)); }
        });
      }
    });
    Object.defineProperty(target, "layerSets", {
      get: function () {
        return collection(children(doc, parent, "sets"), {
          add: function () { return wrap(doc, call("doc.addGroup", doc, parent)); }
        });
      }
    });
  }

  // ---- layers -------------------------------------------------------------
  function defineLayer(C, typename) {
    var p = C.prototype;
    p.typename = typename;
    function info(self) { return call("layer.info", self._doc, self._id); }
    function prop(name, key, write) {
      Object.defineProperty(p, name, {
        get: function () { return info(this)[key]; },
        set: function (v) {
          var patch = {};
          patch[key] = write ? write(v) : v;
          call("layer.set", this._doc, this._id, patch);
        }
      });
    }
    prop("name", "name", String);
    prop("opacity", "opacity", Number);
    prop("fillOpacity", "fillOpacity", Number);
    prop("visible", "visible", Boolean);
    prop("blendMode", "blend", String);
    Object.defineProperty(p, "id", { get: function () { return this._id; } });
    Object.defineProperty(p, "bounds", { get: function () { return info(this).bounds; } });
    Object.defineProperty(p, "isBackgroundLayer", { get: function () { return false; } });
    Object.defineProperty(p, "parent", {
      get: function () {
        var parent = call("layer.parent", this._doc, this._id);
        return parent === null ? new Document(this._doc) : new LayerSet(this._doc, parent);
      }
    });
    p.translate = function (dx, dy) {
      call("layer.translate", this._doc, this._id, Number(dx || 0), Number(dy || 0));
    };
    p.resize = function (sx, sy, anchor) {
      sx = sx === undefined ? 100 : Number(sx);
      sy = sy === undefined ? sx : Number(sy);
      call("layer.scale", this._doc, this._id, sx, sy, anchor || "middlecenter");
    };
    p.rotate = function (angle, anchor) {
      call("layer.rotate", this._doc, this._id, Number(angle || 0), anchor || "middlecenter");
    };
    p.duplicate = function () {
      return wrap(this._doc, call("layer.duplicate", this._doc, this._id));
    };
    p.remove = function () {
      call("layer.remove", this._doc, this._id);
    };
    p.toString = function () { return "[" + typename + " " + this.name + "]"; };
  }

  function ArtLayer(doc, id) {
    this._doc = doc;
    this._id = id;
  }
  defineLayer(ArtLayer, "ArtLayer");
  Object.defineProperty(ArtLayer.prototype, "kind", {
    get: function () { return call("layer.info", this._doc, this._id).kind; },
    set: function (kind) {
      if (kind === "text") {
        this._id = call("layer.toText", this._doc, this._id);
      } else if (kind !== call("layer.info", this._doc, this._id).kind) {
        throw new Error("a layer's kind can only be set to LayerKind.TEXT");
      }
    }
  });
  Object.defineProperty(ArtLayer.prototype, "textItem", {
    get: function () { return new TextItem(this); }
  });
  ArtLayer.prototype.merge = function () {
    return wrap(this._doc, call("layer.mergeDown", this._doc, this._id));
  };

  function LayerSet(doc, id) {
    this._doc = doc;
    this._id = id;
    layerCollections(this, doc, id);
  }
  defineLayer(LayerSet, "LayerSet");
  LayerSet.prototype.merge = function () {
    return wrap(this._doc, call("layer.mergeDown", this._doc, this._id));
  };

  function TextItem(layer) {
    this._layer = layer;
  }
  (function () {
    var p = TextItem.prototype;
    function text(self) {
      var t = call("layer.info", self._layer._doc, self._layer._id).text;
      if (t === null || t === undefined) {
        throw new Error("the layer is not a text layer");
      }
      return t;
    }
    function set(self, patch) {
      call("layer.setText", self._layer._doc, self._layer._id, patch);
    }
    Object.defineProperty(p, "contents", {
      get: function () { return text(this).contents; },
      set: function (v) { set(this, { contents: String(v) }); }
    });
    Object.defineProperty(p, "size", {
      get: function () { return text(this).size; },
      set: function (v) { set(this, { size: Number(v) }); }
    });
    Object.defineProperty(p, "font", {
      get: function () { return text(this).font; },
      set: function (v) { set(this, { font: String(v) }); }
    });
    Object.defineProperty(p, "color", {
      get: function () { return solid(text(this).color); },
      set: function (v) { set(this, { color: rgbOf(v) }); }
    });
    Object.defineProperty(p, "position", {
      get: function () { return text(this).position; },
      set: function (v) { set(this, { position: [Number(v[0]), Number(v[1])] }); }
    });
  })();

  // ---- selection ----------------------------------------------------------
  function Selection(doc) {
    this._doc = doc;
  }
  (function () {
    var p = Selection.prototype;
    p.typename = "Selection";
    p.selectAll = function () { call("sel.all", this._doc); };
    p.deselect = function () { call("sel.none", this._doc); };
    p.invert = function () { call("sel.invert", this._doc); };
    p.select = function (region, type, feather, antiAlias) {
      var points = Array.prototype.map.call(region, function (pt) {
        return [Number(pt[0]), Number(pt[1])];
      });
      call("sel.polygon", this._doc, points, type || "replace", Number(feather || 0));
    };
    p.fill = function (color, mode, opacity, preserveTransparency) {
      var rgb = rgbOf(color === undefined ? g.app.foregroundColor : color);
      call("sel.fill", this._doc, rgb, mode || "normal",
        opacity === undefined ? 100 : Number(opacity), !!preserveTransparency);
    };
    p.clear = function () { call("sel.clear", this._doc); };
    Object.defineProperty(p, "bounds", {
      get: function () { return call("sel.bounds", this._doc); }
    });
    Object.defineProperty(p, "solid", {
      get: function () { return call("sel.bounds", this._doc) !== null; }
    });
  })();

  // ---- documents ----------------------------------------------------------
  function Document(id) {
    this._id = id;
    layerCollections(this, id, null);
  }
  (function () {
    var p = Document.prototype;
    p.typename = "Document";
    function info(self) { return call("doc.info", self._id); }
    ["name", "width", "height", "resolution"].forEach(function (key) {
      Object.defineProperty(p, key, { get: function () { return info(this)[key]; } });
    });
    Object.defineProperty(p, "id", { get: function () { return this._id; } });
    Object.defineProperty(p, "selection", { get: function () { return new Selection(this._id); } });
    Object.defineProperty(p, "activeLayer", {
      get: function () {
        var layer = call("doc.activeLayer", this._id);
        return layer === null ? null : wrap(this._id, layer);
      },
      set: function (layer) { call("doc.setActiveLayer", this._id, layer._id); }
    });
    p.resizeImage = function (width, height, resolution, method) {
      call("doc.resizeImage", this._id, width === undefined ? null : Number(width),
        height === undefined ? null : Number(height), method || "bicubic");
    };
    p.resizeCanvas = function (width, height, anchor) {
      call("doc.resizeCanvas", this._id, Number(width), Number(height), anchor || "middlecenter");
    };
    p.crop = function (bounds) {
      call("doc.crop", this._id, Array.prototype.map.call(bounds, Number));
    };
    p.flatten = function () { call("doc.flatten", this._id); };
    p.mergeVisibleLayers = function () { call("doc.mergeVisible", this._id); };
    p.saveAs = function (file) {
      return call("doc.saveAs", this._id, file ? String(file.name || file) : null);
    };
    p.toString = function () { return "[Document " + this.name + "]"; };
  })();

  // ---- app ----------------------------------------------------------------
  var app = {
    name: "Raster Studio",
    version: consts.version,
    preferences: { rulerUnits: "px", typeUnits: "px" },
    displayDialogs: "no",
    echoToOE: function (s) { call("log", "output", show(s)); },
    open: function (file) {
      var id = call("app.open", file ? String(file.name || file) : null);
      return id === null ? null : new Document(id);
    }
  };
  Object.defineProperty(app, "documents", {
    get: function () {
      return collection(call("app.documents").map(function (id) { return new Document(id); }), {
        add: function (width, height, resolution, name, mode, fill) {
          return new Document(call("app.newDocument",
            width === undefined ? 1280 : Number(width),
            height === undefined ? 720 : Number(height),
            name === undefined ? null : String(name),
            fill || "white"));
        }
      });
    }
  });
  Object.defineProperty(app, "activeDocument", {
    get: function () {
      var id = call("app.activeDocument");
      if (id === null) {
        throw new Error("No document is open");
      }
      return new Document(id);
    },
    set: function (doc) { call("app.setActiveDocument", doc._id); }
  });
  ["foregroundColor", "backgroundColor"].forEach(function (which) {
    Object.defineProperty(app, which, {
      get: function () { return solid(call("app.color", which)); },
      set: function (c) { call("app.setColor", which, rgbOf(c)); }
    });
  });
  g.app = app;
  g.Document = Document;
  g.ArtLayer = ArtLayer;
  g.LayerSet = LayerSet;
})(this);
