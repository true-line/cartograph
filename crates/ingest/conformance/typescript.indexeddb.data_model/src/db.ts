const request = indexedDB.open("shop", 1);
request.onupgradeneeded = () => { request.result.createObjectStore("orders"); };
