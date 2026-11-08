use crate::analyse::log_parser;

pub fn analyse(n: &String) {
    let data = log_parser::read_log_file(n).expect("[ERROR] parse log file failed");

    let mut positive_signal = 0;
    let mut negative_signal = 0;

    for (i, row) in data.iter().enumerate() {
        let pa = row[0];
        let pb = row[1];
        let microprice = row[2];
        let obi = row[3];
        let obi1s = row[4];
        let ofi = row[5];
        let ofi500 = row[6];

        let midprice = (pa + pb) / 2.0;

        // Потенциально сильный сигнал вверх:
        // - OBI положительный и растёт.
        // - OBI 1 sec начинает расти.
        // - OFI 500 ms положительный.
        // - Microprice движется выше midprice.
        if obi > 0.0 && obi1s > 0.0 && ofi500 > 0.0 && (microprice > midprice) {
            positive_signal = positive_signal + 1
        }

        // Потенциально сильный сигнал вниз:
        // - OBI снижается.
        // - OBI 1 sec начинает снижаться.
        // - OFI 500 ms отрицательный.
        // - Microprice приближается к midprice или опускается ниже него.
        if obi < 0.0 && obi1s < 0.0 && ofi500 < 0.0 && (microprice <= midprice) {
            negative_signal = negative_signal + 1
        }

        // Теперь такие сигналы нужно проверить
        // Если сигнал вверх есть и следующие данные по ask и bid сметлись вверх, то такое сигнал можно считать подтвержденным
        // Но если сигнал еще не подтвердился и появился сигнал вниз, то либо сигнал ложный, либо сигнал спекулитивный
        // Также проверять сигнал по timeout, в данной метрики пока будем считать по кол-во рядов
        // Если сигнал вниз есть и следующие данные по ask и bid сметлись вниз, то такое сигнал можно считать подтвержденным
        // Но если сигнал еще не подтвердился и появился сигнал вверх, то либо сигнал ложный, либо сигнал спекулитивный

        // Предварительная гипотеза: твои метрики могли отражать бычий перевес в стакане на этом участке. 
        // Но для подтверждения нужно сопоставить каждый сигнал с последующим изменением цены через 100, 250, 500 мс и 1 секунду.
    }

    println!("[LOG] positive signal count {}", positive_signal);
    println!("[LOG] negativesignal signal count {}", negative_signal);
}
