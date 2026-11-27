let count = 0;
module.exports = { next: () => ++count, file: __filename.split("/").pop(), sep: require("node:path").sep };
