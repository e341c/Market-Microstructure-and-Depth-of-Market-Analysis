use std::{fs, io};

pub fn read_log_file(n: &String) -> io::Result<Vec<Vec<f64>>> {
    let contents = fs::read_to_string(n)?;
    let rows: Vec<&str> = contents.split(";\n").collect();
    let mut data: Vec<Vec<f64>> = Vec::new();
    for row in rows {
        let r: Vec<&str> = row.split("|").collect();
        let mut f_res: Vec<f64> = Vec::new();
        for (i, v) in r.iter().enumerate() {
            if i <= 2 {
                continue;
            }
            let f: f64 = v.trim().parse().unwrap_or(0.0);
            f_res.push(f);
        }
        data.push(f_res);
    }

    data.drain(..1);
    data.drain(data.len() - 1..);
    println!("{:?}", data[0]);
    println!("{:?}", data[data.len() - 1]);

    Ok(data)
}
